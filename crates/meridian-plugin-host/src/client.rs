use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use meridian_sdk::pb::emitter_client::EmitterClient;
use meridian_sdk::pb::importer_client::ImporterClient;
use meridian_sdk::pb::runner_client::RunnerClient;
use meridian_sdk::pb::{self, DescribeRequest};
use meridian_sdk::{
    version, wire, Applied, Artifact, Data, Emitted, ImportSource, Imported, LogLine, Output, Plan,
    ResourceGraph, RunContext, HANDSHAKE_MAGIC, VERSION,
};
use tokio::runtime::Runtime;
use tonic::transport::{Channel, Endpoint};

use crate::{EmitError, Error, ImportFailure, Info, RunFailure, Verification};

/// How long a host waits for a plugin to announce itself. A plugin that has
/// not printed its handshake line by then is either not a Meridian plugin or is
/// wedged; both are better reported than waited on.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a plugin is given to exit once its stdin is closed.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// How much of a plugin's stderr is retained for diagnostics. A plugin in a
/// crash loop can produce a great deal of it, and an unbounded buffer in the
/// host would be a denial of service by a misbehaving target.
const STDERR_LIMIT: usize = 64 << 10;

/// How a plugin is launched.
#[derive(Clone, Debug)]
pub struct StartOptions {
    /// Arguments passed to the plugin binary.
    pub args: Vec<String>,
    /// The plugin's whole environment, replacing the host's. `None` inherits.
    pub env: Option<Vec<(String, String)>>,
    /// How long to wait for the handshake line and the first reply.
    pub handshake_timeout: Duration,
    /// Whether, and against which keys, the binary's signature is checked
    /// before it is started. The default checks nothing.
    pub verification: Verification,
}

impl Default for StartOptions {
    fn default() -> Self {
        StartOptions {
            args: Vec::new(),
            env: None,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            verification: Verification::off(),
        }
    }
}

/// A running plugin process.
///
/// Dropping a client shuts the plugin down. [`Client::close`] does the same
/// and reports whether it went cleanly.
pub struct Client {
    info: Info,
    /// The runtime the connection lives on. It has its own worker thread, so
    /// the connection keeps being serviced whichever thread is calling.
    runtime: Runtime,
    api: Option<EmitterClient<Channel>>,
    /// The same connection, kept for the runner service.
    channel: Option<Channel>,
    process: Option<Process>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Launches a plugin binary and completes the handshake.
    ///
    /// On any failure the process is killed before returning, so a
    /// half-started plugin never survives the error that describes it.
    ///
    /// The binary's signature is checked first, as `options.verification`
    /// says: a plugin that must not run is never started, not started and
    /// then stopped.
    pub fn start(path: &str, options: &StartOptions) -> Result<Client, Error> {
        let signature = options
            .verification
            .check(std::path::Path::new(path))
            .map_err(|cause| Error {
                path: path.to_owned(),
                msg: "the plugin was not started".to_owned(),
                cause,
                ..Error::default()
            })?;
        let process = Process::spawn(path, options)?;
        let deadline = Instant::now() + options.handshake_timeout;

        let handshake = match process.read_handshake(options.handshake_timeout) {
            Ok(handshake) => handshake,
            Err(cause) => return Err(process.fail(path, "handshake failed", cause)),
        };
        // Both checks come before any call, so a plugin this host cannot
        // drive fails here, by version, and not somewhere inside a request.
        if let Err(cause) =
            version::accept(handshake.protocol_version, &version::supported_protocols())
        {
            return Err(process.fail(path, "protocol mismatch", cause));
        }
        if let Err(cause) = version::sdk_compatible(&handshake.sdk_version, VERSION) {
            return Err(process.fail(path, "SDK mismatch", cause));
        }

        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                return Err(process.fail(
                    path,
                    "could not start the connection runtime",
                    e.to_string(),
                ))
            }
        };
        let dial = match handshake.dial() {
            Ok(dial) => dial,
            Err(cause) => return Err(process.fail(path, "could not connect to the plugin", cause)),
        };

        let remaining = deadline.saturating_duration_since(Instant::now());
        let described = runtime.block_on(async {
            let connect_and_describe = async {
                let channel = dial
                    .connect()
                    .await
                    .map_err(|e| ("could not connect to the plugin", describe(&e)))?;
                let mut api = EmitterClient::new(channel.clone());
                let reply = api
                    .describe(DescribeRequest {})
                    .await
                    .map_err(|e| ("the plugin did not describe itself", status(&e)))?
                    .into_inner();
                Ok((api, channel, reply))
            };
            match tokio::time::timeout(remaining, connect_and_describe).await {
                Ok(result) => result,
                Err(_) => Err((
                    "the plugin did not describe itself",
                    format!(
                        "no reply within {:?} of starting it",
                        options.handshake_timeout
                    ),
                )),
            }
        });
        let (api, channel, reply) = match described {
            Ok(pair) => pair,
            Err((msg, cause)) => return Err(process.fail(path, msg, cause)),
        };
        if reply.protocol_version != handshake.protocol_version {
            let cause = format!(
                "the plugin announced protocol version {} and then described itself as \
                 speaking {}",
                handshake.protocol_version, reply.protocol_version
            );
            return Err(process.fail(path, "protocol mismatch", cause));
        }
        if reply.name.is_empty() {
            let cause = "the plugin reported no target name".to_owned();
            return Err(process.fail(path, "the plugin did not describe itself", cause));
        }

        let info = Info {
            // What was agreed in the handshake, which is what both sides
            // now speak.
            protocol_version: handshake.protocol_version,
            sdk_version: reply.sdk_version,
            name: reply.name,
            resource_types: reply.resource_types,
            capabilities: wire::capabilities_from_wire(reply.capabilities),
            category: wire::category_from_wire(reply.category),
            target_versions: reply.target_versions,
            resource_schemas: reply
                .resource_schemas
                .into_iter()
                .map(wire::resource_schema_from_wire)
                .collect(),
            operations: wire::operations_from_wire(reply.operations),
            accepts_inputs: reply.accepts_inputs,
            import_formats: wire::import_formats_from_wire(reply.import_formats),
            path: path.to_owned(),
            signature,
        };
        Ok(Client {
            info,
            runtime,
            api: Some(api),
            channel: Some(channel),
            process: Some(process),
        })
    }

    /// What the plugin reported when it was started.
    pub fn info(&self) -> &Info {
        &self.info
    }

    /// The target name documents select with `spec.target`.
    pub fn name(&self) -> &str {
        &self.info.name
    }

    /// Compiles a tree in the plugin process. Safe to call from several
    /// threads at once.
    pub fn emit(&self, graph: &ResourceGraph, data: &Data) -> Result<Emitted, EmitError> {
        let fault = |msg: &str, cause: String, stderr: String| {
            EmitError::Plugin(Error {
                plugin: self.info.name.clone(),
                path: self.info.path.clone(),
                msg: msg.to_owned(),
                cause,
                stderr,
            })
        };
        let Some(api) = &self.api else {
            return Err(fault(
                "the plugin has been shut down",
                String::new(),
                String::new(),
            ));
        };
        let mut api = api.clone();
        let request = wire::emit_request(graph, data);
        match self.runtime.block_on(api.emit(request)) {
            Ok(reply) => wire::emit_response_from_wire(reply.into_inner(), &self.info.name)
                .map_err(EmitError::Refused),
            Err(e) => {
                // A plugin that failed mid-call is usually still exiting, so
                // give it a moment to finish saying why.
                let stderr = self
                    .process
                    .as_ref()
                    .map(|p| p.stderr_after(Duration::from_secs(1)))
                    .unwrap_or_default();
                Err(fault(
                    "the plugin failed while compiling",
                    status(&e),
                    stderr,
                ))
            }
        }
    }

    /// Reads a tool's existing configuration into Meridian documents. A
    /// plugin that listed no import formats is never asked.
    pub fn import(&self, source: &ImportSource) -> Result<Imported, ImportFailure> {
        let fault = |msg: String, cause: String, stderr: String| {
            ImportFailure::Plugin(Error {
                plugin: self.info.name.clone(),
                path: self.info.path.clone(),
                msg,
                cause,
                stderr,
            })
        };
        if self.info.import_formats.is_empty() {
            return Err(fault(
                "this target does not import".to_owned(),
                String::new(),
                String::new(),
            ));
        }
        let Some(channel) = &self.channel else {
            return Err(fault(
                "the plugin has been shut down".to_owned(),
                String::new(),
                String::new(),
            ));
        };
        let mut importer = ImporterClient::new(channel.clone());
        let request = wire::import_request(source);
        let reply = match self.runtime.block_on(importer.import(request)) {
            Ok(reply) => reply.into_inner(),
            Err(e) => {
                let stderr = self
                    .process
                    .as_ref()
                    .map(|p| p.stderr_after(Duration::from_secs(1)))
                    .unwrap_or_default();
                return Err(fault(
                    "the plugin failed while importing".to_owned(),
                    status(&e),
                    stderr,
                ));
            }
        };
        let mut imported = wire::import_response_from_wire(reply)
            .map_err(|cause| {
                fault(
                    "the plugin sent a result this host cannot read while importing".to_owned(),
                    cause,
                    String::new(),
                )
            })?
            .map_err(ImportFailure::Source)?;
        for document in &mut imported.documents {
            if document.target.is_empty() {
                document.target = self.info.name.clone();
            }
        }
        Ok(imported)
    }

    /// Previews what applying an artifact would change. `log` receives the
    /// tool's output line by line while it runs.
    pub fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &mut dyn FnMut(LogLine),
    ) -> Result<Plan, RunFailure> {
        let mut runner = self.runner("plan", self.info.operations.plan)?;
        let request = pb::PlanRequest {
            context: Some(wire::context_to_wire(context)),
            artifact: Some(wire::artifact_to_wire(artifact)),
        };
        let result = self.runtime.block_on(async {
            let mut events = runner.plan(request).await?.into_inner();
            let mut result = None;
            while let Some(event) = events.message().await? {
                match event.event {
                    Some(pb::plan_event::Event::Log(line)) => log(wire::log_from_wire(line)),
                    Some(pb::plan_event::Event::Result(r)) => result = Some(r),
                    None => {}
                }
            }
            Ok::<_, tonic::Status>(result)
        });
        let result = self.finished("planning", result)?;
        wire::plan_result_from_wire(result)
            .map_err(|e| self.unreadable("planning", e))?
            .map_err(RunFailure::Tool)
    }

    /// Makes an artifact's desired state real. `log` receives the tool's
    /// output line by line while it runs.
    pub fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &mut dyn FnMut(LogLine),
    ) -> Result<Applied, RunFailure> {
        let mut runner = self.runner("apply", self.info.operations.apply)?;
        let request = pb::ApplyRequest {
            context: Some(wire::context_to_wire(context)),
            artifact: Some(wire::artifact_to_wire(artifact)),
        };
        let result = self.runtime.block_on(async {
            let mut events = runner.apply(request).await?.into_inner();
            let mut result = None;
            while let Some(event) = events.message().await? {
                match event.event {
                    Some(pb::apply_event::Event::Log(line)) => log(wire::log_from_wire(line)),
                    Some(pb::apply_event::Event::Result(r)) => result = Some(r),
                    None => {}
                }
            }
            Ok::<_, tonic::Status>(result)
        });
        let result = self.finished("applying", result)?;
        wire::apply_result_from_wire(result)
            .map_err(|e| self.unreadable("applying", e))?
            .map_err(RunFailure::Tool)
    }

    /// Reads the values the last apply produced, without applying anything.
    pub fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunFailure> {
        let mut runner = self.runner("report outputs", self.info.operations.outputs)?;
        let request = pb::OutputsRequest {
            context: Some(wire::context_to_wire(context)),
        };
        let result = self
            .runtime
            .block_on(runner.outputs(request))
            .map(|r| Some(r.into_inner()));
        let result = self.finished("reading outputs", result)?;
        wire::outputs_response_from_wire(result)
            .map_err(|e| self.unreadable("reading outputs", e))?
            .map_err(RunFailure::Tool)
    }

    /// The runner connection, if the plugin declared the operation. A plugin
    /// is never asked for something it did not offer: the declaration is the
    /// contract, and probing past it would make it meaningless.
    fn runner(&self, verb: &str, offered: bool) -> Result<RunnerClient<Channel>, RunFailure> {
        let fault = |msg: String| {
            RunFailure::Plugin(Error {
                plugin: self.info.name.clone(),
                path: self.info.path.clone(),
                msg,
                ..Error::default()
            })
        };
        if !offered {
            return Err(fault(format!("this target does not {verb}")));
        }
        match &self.channel {
            Some(channel) => Ok(RunnerClient::new(channel.clone())),
            None => Err(fault("the plugin has been shut down".to_owned())),
        }
    }

    /// Turns the end of a run's stream into its result, or into the plugin
    /// failure that a missing result is.
    fn finished<T>(
        &self,
        doing: &str,
        outcome: Result<Option<T>, tonic::Status>,
    ) -> Result<T, RunFailure> {
        let (cause, wait) = match outcome {
            Ok(Some(result)) => return Ok(result),
            Ok(None) => (
                "the plugin ended the run without reporting a result".to_owned(),
                Duration::ZERO,
            ),
            // A plugin that failed mid-run is usually still exiting, so give
            // it a moment to finish saying why.
            Err(e) => (status(&e), Duration::from_secs(1)),
        };
        let stderr = self
            .process
            .as_ref()
            .map(|p| p.stderr_after(wait))
            .unwrap_or_default();
        Err(RunFailure::Plugin(Error {
            plugin: self.info.name.clone(),
            path: self.info.path.clone(),
            msg: format!("the plugin failed while {doing}"),
            cause,
            stderr,
        }))
    }

    fn unreadable(&self, doing: &str, cause: String) -> RunFailure {
        RunFailure::Plugin(Error {
            plugin: self.info.name.clone(),
            path: self.info.path.clone(),
            msg: format!("the plugin sent a result this host cannot read while {doing}"),
            cause,
            ..Error::default()
        })
    }

    /// Shuts the plugin down and waits for it to exit.
    pub fn close(mut self) -> Result<(), Error> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<(), Error> {
        self.api = None;
        self.channel = None;
        let Some(process) = self.process.take() else {
            return Ok(());
        };
        if process.stop(EXIT_GRACE) {
            return Ok(());
        }
        Err(Error {
            plugin: self.info.name.clone(),
            path: self.info.path.clone(),
            msg: "did not exit when asked and was killed".to_owned(),
            ..Error::default()
        })
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// A transport error's whole chain of causes. Its own message is usually just
/// "transport error"; the reason is further down.
fn describe(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(inner) = source {
        out.push_str(": ");
        out.push_str(&inner.to_string());
        source = inner.source();
    }
    out
}

fn status(s: &tonic::Status) -> String {
    format!("{:?}: {}", s.code(), s.message())
}

/// The parsed form of the plugin's announcement line,
/// `MAGIC|protocol|sdk|network|address`.
#[derive(Debug, PartialEq, Eq)]
struct Handshake {
    protocol_version: u32,
    sdk_version: String,
    network: String,
    address: String,
}

impl Handshake {
    fn parse(line: &str) -> Result<Handshake, String> {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() != 5 || parts[0] != HANDSHAKE_MAGIC {
            return Err(format!(
                "the first line of output was {:?}, which is not a Meridian plugin handshake",
                truncate(line, 200)
            ));
        }
        let Ok(protocol_version) = parts[1].parse::<u32>() else {
            return Err(format!(
                "the handshake reported protocol version {:?}, which is not a number",
                parts[1]
            ));
        };
        if parts[4].is_empty() {
            return Err("the handshake reported no address to dial".to_owned());
        }
        Ok(Handshake {
            protocol_version,
            sdk_version: parts[2].to_owned(),
            network: parts[3].to_owned(),
            address: parts[4].to_owned(),
        })
    }

    fn dial(&self) -> Result<Dial, String> {
        let uri = match self.network.as_str() {
            "unix" => format!("unix:{}", self.address),
            "tcp" | "tcp4" | "tcp6" => format!("http://{}", self.address),
            #[cfg(windows)]
            "npipe" => return Ok(Dial::Pipe(self.address.clone())),
            other => {
                return Err(format!(
                    "the handshake named network {other:?}, which this host cannot dial"
                ))
            }
        };
        Endpoint::from_shared(uri)
            .map(|endpoint| Dial::Endpoint(Box::new(endpoint)))
            .map_err(|e| describe(&e))
    }
}

/// How to reach a plugin, from its handshake.
#[derive(Debug)]
enum Dial {
    /// A Unix socket or a TCP address, which the transport dials by itself.
    Endpoint(Box<Endpoint>),
    /// A Windows named pipe, by its full name.
    #[cfg(windows)]
    Pipe(String),
}

impl Dial {
    async fn connect(self) -> Result<Channel, tonic::transport::Error> {
        match self {
            Dial::Endpoint(endpoint) => endpoint.connect().await,
            #[cfg(windows)]
            Dial::Pipe(name) => {
                // The address is never dialled: every connection goes to the
                // pipe. The transport only needs something shaped like one.
                Endpoint::from_static("http://meridian-plugin.invalid")
                    .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                        let name = name.clone();
                        async move { open_pipe(&name).await.map(hyper_util::rt::TokioIo::new) }
                    }))
                    .await
            }
        }
    }
}

/// Opens a plugin's named pipe, waiting out the moment between one
/// connection being taken and the plugin offering the next.
#[cfg(windows)]
async fn open_pipe(
    name: &str,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    const PIPE_BUSY: i32 = 231;
    let mut attempt = 0;
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(name) {
            Err(err) if err.raw_os_error() == Some(PIPE_BUSY) && attempt < 200 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            result => return result,
        }
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}...", &s[..cut]),
        None => s.to_owned(),
    }
}

/// The first [`STDERR_LIMIT`] bytes a plugin wrote. The first bytes, not the
/// last: a startup failure explains itself at the top, and truncating the head
/// would throw away the sentence that matters.
#[derive(Default)]
struct Head {
    buf: Vec<u8>,
    over: bool,
}

impl Head {
    fn write(&mut self, bytes: &[u8]) {
        let room = STDERR_LIMIT - self.buf.len();
        self.buf.extend_from_slice(&bytes[..bytes.len().min(room)]);
        self.over |= bytes.len() > room;
    }

    fn text(&self) -> String {
        let mut s = String::from_utf8_lossy(&self.buf).trim().to_owned();
        if self.over {
            s.push_str("\n... (truncated)");
        }
        s
    }
}

/// The plugin's operating-system process and the pipes to it.
struct Process {
    child: Child,
    /// Holding the write end of the plugin's stdin is how the host says "still
    /// here": closing it is what tells the plugin to exit.
    stdin: Option<ChildStdin>,
    /// The first line the plugin wrote to stdout, or why there was none.
    ///
    /// The receivers sit behind mutexes only so a client can be shared between
    /// threads; nothing contends for them.
    first_line: Mutex<Receiver<Result<String, String>>>,
    stderr: Arc<Mutex<Head>>,
    /// Signals once every byte the plugin wrote to stderr has been collected.
    stderr_done: Mutex<Receiver<()>>,
}

impl Process {
    fn spawn(path: &str, options: &StartOptions) -> Result<Process, Error> {
        let mut command = Command::new(path);
        command
            .args(&options.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(env) = &options.env {
            command.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        // The offer the plugin picks its protocol version from. Set after
        // any replacement environment, so a caller cannot drop it by accident.
        command.env(
            version::PROTOCOLS_ENV,
            version::offer(&version::supported_protocols()),
        );
        let mut child = spawn_when_ready(&mut command).map_err(|e| Error {
            path: path.to_owned(),
            msg: "could not start the plugin".to_owned(),
            cause: e.to_string(),
            ..Error::default()
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout was piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr was piped");

        let stderr = Arc::new(Mutex::new(Head::default()));
        let (done_tx, stderr_done) = mpsc::channel();
        let collected = Arc::clone(&stderr);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n @ 1..) = stderr_pipe.read(&mut chunk) {
                collected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .write(&chunk[..n]);
            }
            let _ = done_tx.send(());
        });

        // The read runs on its own thread so a plugin that prints nothing is
        // bounded by the timeout instead of hanging the host forever.
        let (line_tx, first_line) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            // Bounded, so a binary that writes megabytes without a newline
            // cannot make the host hold all of it.
            let outcome = match (&mut reader).take(64 << 10).read_until(b'\n', &mut line) {
                Ok(0) => Err("the plugin exited before announcing itself".to_owned()),
                Ok(_) => Ok(String::from_utf8_lossy(&line)
                    .trim_end_matches(['\r', '\n'])
                    .to_owned()),
                Err(e) => Err(format!("the plugin exited before announcing itself: {e}")),
            };
            let _ = line_tx.send(outcome);
            // Keep reading, so a plugin that writes to stdout later is never
            // blocked on a full pipe.
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });

        Ok(Process {
            child,
            stdin,
            first_line: Mutex::new(first_line),
            stderr,
            stderr_done: Mutex::new(stderr_done),
        })
    }

    fn read_handshake(&self, timeout: Duration) -> Result<Handshake, String> {
        let first_line = self.first_line.lock().unwrap_or_else(|e| e.into_inner());
        match first_line.recv_timeout(timeout) {
            Ok(Ok(line)) => Handshake::parse(&line),
            Ok(Err(why)) => Err(why),
            Err(_) => Err(format!("no handshake line within {timeout:?}")),
        }
    }

    /// What the plugin has written to stderr, waiting up to `grace` for it to
    /// finish writing.
    fn stderr_after(&self, grace: Duration) -> String {
        let _ = self
            .stderr_done
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recv_timeout(grace);
        self.stderr.lock().unwrap_or_else(|e| e.into_inner()).text()
    }

    /// Kills the process and returns the error describing why.
    fn fail(mut self, path: &str, msg: &str, cause: String) -> Error {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Bounded, so a plugin that leaves the pipe open in a child process of
        // its own cannot wedge the host's error path.
        let stderr = self.stderr_after(Duration::from_secs(1));
        Error {
            plugin: String::new(),
            path: path.to_owned(),
            msg: msg.to_owned(),
            cause,
            stderr,
        }
    }

    /// Asks the plugin to exit by closing its stdin, and kills it if it has
    /// not gone within `grace`. Returns whether it exited on its own.
    fn stop(mut self, grace: Duration) -> bool {
        self.stdin = None;
        let deadline = Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                // Being stopped on request is not a failure, whatever the status.
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return false;
                }
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Reached only if a process is dropped without being stopped or
        // failed, which is a panic unwinding. Do not leave it running.
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// One plugin serves every document compiled for its target, so the client has
/// to be usable from whichever thread is compiling.
const _: fn() = || {
    fn shareable<T: Send + Sync>() {}
    shareable::<Client>();
};

/// Starts a process, trying again when the program is briefly "busy".
///
/// A program that was written a moment ago cannot be run while any process
/// still holds it open for writing, and a process another thread is in the
/// middle of starting holds every open file for an instant. The condition
/// clears by itself, so it is waited out instead of reported.
fn spawn_when_ready(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    // "Text file busy" on Unix, and a sharing violation on Windows.
    const BUSY: i32 = if cfg!(windows) { 32 } else { 26 };
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(err) if err.raw_os_error() == Some(BUSY) && attempt < 50 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handshake_line_parses() {
        let h = Handshake::parse("MERIDIAN-PLUGIN|1|0.1.0|unix|/tmp/meridian-plugin-1/plugin.sock")
            .unwrap();
        assert_eq!(
            h,
            Handshake {
                protocol_version: 1,
                sdk_version: "0.1.0".to_owned(),
                network: "unix".to_owned(),
                address: "/tmp/meridian-plugin-1/plugin.sock".to_owned(),
            }
        );
        assert!(h.dial().is_ok());
        assert!(
            Handshake::parse("MERIDIAN-PLUGIN|1|0.1.0|tcp|127.0.0.1:4000")
                .unwrap()
                .dial()
                .is_ok()
        );
        // A named pipe is something only a Windows host can open.
        let pipe = Handshake::parse(r"MERIDIAN-PLUGIN|1|0.1.0|npipe|\\.\pipe\meridian-plugin-1")
            .unwrap()
            .dial();
        assert_eq!(pipe.is_ok(), cfg!(windows));
    }

    #[test]
    fn a_malformed_handshake_says_what_is_wrong() {
        for (line, want) in [
            ("hello world", "the first line of output was \"hello world\", which is not a Meridian plugin handshake"),
            ("OTHER|1|0.1.0|unix|/x", "is not a Meridian plugin handshake"),
            ("MERIDIAN-PLUGIN|1|0.1.0|unix", "is not a Meridian plugin handshake"),
            ("MERIDIAN-PLUGIN|one|0.1.0|unix|/x", "protocol version \"one\", which is not a number"),
            ("MERIDIAN-PLUGIN|1|0.1.0|unix|", "no address to dial"),
        ] {
            let err = Handshake::parse(line).unwrap_err();
            assert!(err.contains(want), "{line:?}: error = {err}");
        }
        let err = Handshake::parse("MERIDIAN-PLUGIN|1|0.1.0|carrier-pigeon|coop")
            .unwrap()
            .dial()
            .unwrap_err();
        assert!(err.contains("network \"carrier-pigeon\""), "{err}");
        // A long first line is quoted only in part.
        let err = Handshake::parse(&"x".repeat(5000)).unwrap_err();
        assert!(err.len() < 400, "{}", err.len());
    }

    #[test]
    fn retained_stderr_keeps_the_head_and_says_when_it_was_cut() {
        let mut head = Head::default();
        head.write(b"  first line\n");
        assert_eq!(head.text(), "first line");
        head.write(&vec![b'x'; STDERR_LIMIT]);
        head.write(b"more");
        let text = head.text();
        assert!(text.starts_with("first line\nxxx"));
        assert!(text.ends_with("\n... (truncated)"));
        assert_eq!(head.buf.len(), STDERR_LIMIT);
    }
}
