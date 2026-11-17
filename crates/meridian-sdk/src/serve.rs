//! The plugin's side of the process boundary.
//!
//! [`serve`] is everything a plugin's `main` needs. It listens on a private
//! socket, announces where on stdout, answers the host until the host closes
//! the plugin's stdin, and then exits. A plugin author writes an [`Emitter`]
//! and perhaps a [`Runner`], and never touches the protocol.

use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use crate::pb::emitter_server::{Emitter as EmitterService, EmitterServer};
use crate::pb::importer_server::{Importer as ImporterService, ImporterServer};
use crate::pb::runner_server::{Runner as RunnerService, RunnerServer};
use crate::{
    pb, version, wire, Emitter, Importer, Log, LogLine, Operations, Runner, HANDSHAKE_MAGIC,
    VERSION,
};

/// What a plugin binary serves: an emitter, and optionally a runner and an
/// importer.
pub struct Plugin {
    emitter: Arc<dyn Emitter + Send + Sync>,
    runner: Option<Arc<dyn Runner>>,
    importer: Option<Arc<dyn Importer>>,
}

impl Plugin {
    /// A plugin that compiles.
    pub fn new(emitter: impl Emitter + Send + Sync + 'static) -> Self {
        Plugin {
            emitter: Arc::new(emitter),
            runner: None,
            importer: None,
        }
    }

    /// Adds the half that reads the tool's existing configuration back into
    /// Meridian documents.
    pub fn with_importer(mut self, importer: impl Importer + 'static) -> Self {
        self.importer = Some(Arc::new(importer));
        self
    }

    /// Adds the half that drives the real tool.
    pub fn with_runner(mut self, runner: impl Runner + 'static) -> Self {
        self.runner = Some(Arc::new(runner));
        self
    }

    fn operations(&self) -> Operations {
        self.runner
            .as_ref()
            .map(|r| r.operations())
            .unwrap_or(Operations::COMPILE_ONLY)
    }
}

/// Why a plugin could not serve.
#[derive(Debug)]
pub struct ServeError {
    msg: String,
}

impl ServeError {
    fn new(what: &str, cause: impl fmt::Display) -> Self {
        ServeError {
            msg: format!("{what}: {cause}"),
        }
    }
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for ServeError {}

/// Serves a plugin to the host that launched this process, and returns when
/// the host is done with it.
///
/// Stdout belongs to the protocol: the handshake line is written there and
/// nothing else may be. A plugin that wants to say something writes to stderr,
/// which the host keeps for diagnostics.
pub fn serve(plugin: Plugin) -> Result<(), ServeError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| ServeError::new("starting the plugin runtime", e))?;
    runtime.block_on(serve_async(plugin))
}

async fn serve_async(plugin: Plugin) -> Result<(), ServeError> {
    let socket = Socket::create()?;
    let listener = UnixListener::bind(&socket.path)
        .map_err(|e| ServeError::new("listening on the plugin socket", e))?;

    // Announce only once the socket is listening, so the host can dial the
    // moment it has read the line.
    //
    // The version announced is the highest the host offered that this SDK
    // also speaks. A host that offered nothing predates negotiation, and gets
    // this SDK's newest, as it always did.
    let offered = std::env::var(version::PROTOCOLS_ENV).ok();
    let protocol = version::choose(offered.as_deref(), &version::supported_protocols());
    {
        let mut stdout = std::io::stdout().lock();
        writeln!(
            stdout,
            "{HANDSHAKE_MAGIC}|{protocol}|{VERSION}|unix|{}",
            socket.path.display()
        )
        .and_then(|()| stdout.flush())
        .map_err(|e| ServeError::new("writing the plugin handshake", e))?;
    }

    let operations = plugin.operations();
    let emitter = EmitterServer::new(EmitterHandler {
        emitter: Arc::clone(&plugin.emitter),
        operations,
        accepts_inputs: plugin.runner.as_ref().is_some_and(|r| r.accepts_inputs()),
        importer: plugin.importer.clone(),
        protocol,
    });
    let mut router = tonic::transport::Server::builder().add_service(emitter);
    if let Some(runner) = &plugin.runner {
        router = router.add_service(RunnerServer::new(RunnerHandler {
            runner: Arc::clone(runner),
        }));
    }
    if let Some(importer) = &plugin.importer {
        router = router.add_service(ImporterServer::new(ImporterHandler {
            importer: Arc::clone(importer),
        }));
    }

    router
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), host_is_done())
        .await
        .map_err(|e| ServeError::new("serving the plugin", e))
}

/// Resolves when the host no longer wants the plugin: it closed the plugin's
/// stdin, which also happens if the host dies, or the process was signalled.
/// A plugin that outlived its host would be an orphan nobody is watching.
async fn host_is_done() {
    let (tx, stdin_closed) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        let mut sink = [0u8; 256];
        let mut stdin = std::io::stdin().lock();
        while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
        let _ = tx.send(());
    });

    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };

    tokio::select! {
        _ = stdin_closed => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate => {}
    }
}

/// The plugin's socket, in a directory only this user can enter. The
/// directory is removed when the plugin stops serving.
struct Socket {
    dir: PathBuf,
    path: PathBuf,
}

impl Socket {
    fn create() -> Result<Socket, ServeError> {
        use std::os::unix::fs::DirBuilderExt;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        // A unix socket's path has a small fixed limit, and a temporary
        // directory can be deep enough to exceed it (macOS's is, and so is a
        // CI runner's). When it is, fall back to /tmp, where the path is short.
        let bases = [std::env::temp_dir(), PathBuf::from("/tmp")];
        let mut last = String::from("no usable temporary directory");
        for base in bases {
            // The name only has to be unused; what keeps other users out is
            // the directory's mode, and creating it fails if anything is
            // already there, so a guessed name cannot be used against the
            // plugin.
            for attempt in 0..16u32 {
                let dir = base.join(format!(
                    "meridian-plugin-{}-{:08x}",
                    std::process::id(),
                    nanos.wrapping_add(attempt)
                ));
                let path = dir.join("plugin.sock");
                if path.as_os_str().len() > SOCKET_PATH_LIMIT {
                    last = format!("{} is too long a path for a unix socket", path.display());
                    break;
                }
                match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                    Ok(()) => return Ok(Socket { dir, path }),
                    Err(e) => last = format!("{}: {e}", dir.display()),
                }
            }
        }
        Err(ServeError::new(
            "creating a directory for the plugin socket",
            last,
        ))
    }
}

/// The longest socket path used. `sockaddr_un` holds 104 bytes on macOS and
/// 108 on Linux, including a terminator; this stays under both.
const SOCKET_PATH_LIMIT: usize = 100;

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct EmitterHandler {
    emitter: Arc<dyn Emitter + Send + Sync>,
    operations: Operations,
    accepts_inputs: bool,
    /// Kept only to describe it: importing is served by its own handler.
    importer: Option<Arc<dyn Importer>>,
    /// The protocol version agreed with the host.
    protocol: u32,
}

#[tonic::async_trait]
impl EmitterService for EmitterHandler {
    async fn describe(
        &self,
        _request: Request<pb::DescribeRequest>,
    ) -> Result<Response<pb::DescribeResponse>, Status> {
        let mut resource_types = self.emitter.supported_resource_types();
        resource_types.sort();
        Ok(Response::new(pb::DescribeResponse {
            protocol_version: self.protocol,
            sdk_version: VERSION.to_owned(),
            name: self.emitter.name().to_owned(),
            resource_types,
            capabilities: Some(wire::capabilities_to_wire(self.emitter.capabilities())),
            category: wire::category_to_wire(self.emitter.category()) as i32,
            target_versions: self.emitter.target_versions(),
            operations: Some(wire::operations_to_wire(self.operations)),
            accepts_inputs: self.accepts_inputs,
            import_formats: wire::import_formats_to_wire(
                self.importer
                    .as_ref()
                    .map(|i| i.formats())
                    .unwrap_or_default(),
            ),
        }))
    }

    async fn emit(
        &self,
        request: Request<pb::EmitRequest>,
    ) -> Result<Response<pb::EmitResponse>, Status> {
        let (graph, data) =
            wire::emit_request_from_wire(request.into_inner()).map_err(Status::invalid_argument)?;
        let emitter = Arc::clone(&self.emitter);
        // An emitter is ordinary blocking code, and a panic in it must reach
        // the host as this call failing, not take the whole plugin down.
        let outcome = tokio::task::spawn_blocking(move || emitter.emit(&graph, &data))
            .await
            .map_err(|e| Status::internal(format!("the emitter panicked: {e}")))?;
        Ok(Response::new(wire::emit_response(outcome)))
    }
}

struct ImporterHandler {
    importer: Arc<dyn Importer>,
}

#[tonic::async_trait]
impl ImporterService for ImporterHandler {
    async fn import(
        &self,
        request: Request<pb::ImportRequest>,
    ) -> Result<Response<pb::ImportResponse>, Status> {
        let source = wire::import_request_from_wire(request.into_inner());
        let importer = Arc::clone(&self.importer);
        // An importer parses whatever it is handed, so a panic in it must
        // reach the host as this call failing, not take the plugin down.
        let outcome = tokio::task::spawn_blocking(move || importer.import(&source))
            .await
            .map_err(|e| Status::internal(format!("the importer panicked: {e}")))?;
        Ok(Response::new(wire::import_response(outcome)))
    }
}

struct RunnerHandler {
    runner: Arc<dyn Runner>,
}

type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

/// Relays a runner's log lines into the response stream as they are written.
struct StreamLog<T> {
    events: mpsc::UnboundedSender<Result<T, Status>>,
    wrap: fn(pb::LogLine) -> T,
}

impl<T: Send> Log for StreamLog<T> {
    fn line(&self, line: LogLine) {
        // The host hanging up mid-run is not the runner's problem: the tool
        // is already running and should be left to finish.
        let _ = self.events.send(Ok((self.wrap)(wire::log_to_wire(line))));
    }
}

/// Runs a blocking runner call, streaming its log lines and then its result.
fn run_streaming<T, R>(
    wrap_log: fn(pb::LogLine) -> T,
    wrap_result: fn(R) -> T,
    call: impl FnOnce(&dyn Log) -> R + Send + 'static,
) -> EventStream<T>
where
    T: Send + 'static,
    R: Send + 'static,
{
    let (events, stream) = mpsc::unbounded_channel();
    let (bounded, out) = mpsc::channel(64);
    // The unbounded side keeps `Log::line` non-blocking for the runner; this
    // task applies the transport's backpressure between the two.
    tokio::spawn(async move {
        let mut stream = stream;
        while let Some(event) = stream.recv().await {
            if bounded.send(event).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let log = StreamLog {
            events: events.clone(),
            wrap: wrap_log,
        };
        let outcome = tokio::task::spawn_blocking(move || call(&log)).await;
        let last = match outcome {
            Ok(result) => Ok(wrap_result(result)),
            Err(e) => Err(Status::internal(format!("the runner panicked: {e}"))),
        };
        let _ = events.send(last);
    });
    Box::pin(ReceiverStream::new(out))
}

fn plan_log(line: pb::LogLine) -> pb::PlanEvent {
    pb::PlanEvent {
        event: Some(pb::plan_event::Event::Log(line)),
    }
}

fn plan_done(result: pb::PlanResult) -> pb::PlanEvent {
    pb::PlanEvent {
        event: Some(pb::plan_event::Event::Result(result)),
    }
}

fn apply_log(line: pb::LogLine) -> pb::ApplyEvent {
    pb::ApplyEvent {
        event: Some(pb::apply_event::Event::Log(line)),
    }
}

fn apply_done(result: pb::ApplyResult) -> pb::ApplyEvent {
    pb::ApplyEvent {
        event: Some(pb::apply_event::Event::Result(result)),
    }
}

#[tonic::async_trait]
impl RunnerService for RunnerHandler {
    type PlanStream = EventStream<pb::PlanEvent>;
    type ApplyStream = EventStream<pb::ApplyEvent>;

    async fn plan(
        &self,
        request: Request<pb::PlanRequest>,
    ) -> Result<Response<Self::PlanStream>, Status> {
        if !self.runner.operations().plan {
            return Err(Status::unimplemented("this plugin does not plan"));
        }
        let request = request.into_inner();
        let context = wire::context_from_wire(request.context).map_err(Status::invalid_argument)?;
        let artifact = wire::artifact_from_wire(request.artifact);
        let runner = Arc::clone(&self.runner);
        Ok(Response::new(run_streaming(
            plan_log,
            plan_done,
            move |log| wire::plan_result(runner.plan(&context, &artifact, log)),
        )))
    }

    async fn apply(
        &self,
        request: Request<pb::ApplyRequest>,
    ) -> Result<Response<Self::ApplyStream>, Status> {
        if !self.runner.operations().apply {
            return Err(Status::unimplemented("this plugin does not apply"));
        }
        let request = request.into_inner();
        let context = wire::context_from_wire(request.context).map_err(Status::invalid_argument)?;
        let artifact = wire::artifact_from_wire(request.artifact);
        let runner = Arc::clone(&self.runner);
        Ok(Response::new(run_streaming(
            apply_log,
            apply_done,
            move |log| wire::apply_result(runner.apply(&context, &artifact, log)),
        )))
    }

    async fn outputs(
        &self,
        request: Request<pb::OutputsRequest>,
    ) -> Result<Response<pb::OutputsResponse>, Status> {
        if !self.runner.operations().outputs {
            return Err(Status::unimplemented("this plugin does not report outputs"));
        }
        let context = wire::context_from_wire(request.into_inner().context)
            .map_err(Status::invalid_argument)?;
        let runner = Arc::clone(&self.runner);
        let outcome = tokio::task::spawn_blocking(move || runner.outputs(&context))
            .await
            .map_err(|e| Status::internal(format!("the runner panicked: {e}")))?;
        Ok(Response::new(wire::outputs_response(outcome)))
    }
}
