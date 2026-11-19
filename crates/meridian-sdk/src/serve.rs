//! The plugin's side of the process boundary.
//!
//! [`serve`] is everything a plugin's `main` needs. It listens on a private
//! endpoint (a Unix socket, or a named pipe on Windows), announces where on stdout, answers the host until the host closes
//! the plugin's stdin, and then exits. A plugin author writes an [`Emitter`]
//! and perhaps a [`Runner`], and never touches the protocol.

use std::fmt;
use std::io::{Read, Write};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use crate::listen::Listening;
use crate::pb::emitter_server::{Emitter as EmitterService, EmitterServer};
use crate::pb::importer_server::{Importer as ImporterService, ImporterServer};
use crate::pb::runner_server::{Runner as RunnerService, RunnerServer};
use crate::pb::schema_source_server::{SchemaSource as SchemaSourceService, SchemaSourceServer};
use crate::{
    pb, version, wire, Emitter, Importer, Log, LogLine, LogStream, Operations, Runner,
    SchemaSource, HANDSHAKE_MAGIC, VERSION,
};

/// What a plugin binary serves: an emitter, and optionally a runner, an
/// importer and a schema source.
pub struct Plugin {
    emitter: Arc<dyn Emitter + Send + Sync>,
    runner: Option<Arc<dyn Runner>>,
    importer: Option<Arc<dyn Importer>>,
    schemas: Option<Arc<dyn SchemaSource>>,
}

impl Plugin {
    /// A plugin that compiles.
    pub fn new(emitter: impl Emitter + Send + Sync + 'static) -> Self {
        Plugin {
            emitter: Arc::new(emitter),
            runner: None,
            importer: None,
            schemas: None,
        }
    }

    /// Adds the half that asks the installed tool for the schemas of its
    /// own resource types, for `meridian schema sync`.
    pub fn with_schema_source(mut self, source: impl SchemaSource + 'static) -> Self {
        self.schemas = Some(Arc::new(source));
        self
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
    pub(crate) fn new(what: &str, cause: impl fmt::Display) -> Self {
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
    let listening = Listening::start()?;

    // Announce only once the endpoint is listening, so the host can dial the
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
            "{HANDSHAKE_MAGIC}|{protocol}|{VERSION}|{}|{}",
            Listening::NETWORK,
            listening.address
        )
        .and_then(|()| stdout.flush())
        .map_err(|e| ServeError::new("writing the plugin handshake", e))?;
    }

    let operations = plugin.operations();
    let emitter = EmitterServer::new(EmitterHandler {
        emitter: Arc::clone(&plugin.emitter),
        operations,
        accepts_inputs: plugin.runner.as_ref().is_some_and(|r| r.accepts_inputs()),
        saves_plans: plugin.runner.as_ref().is_some_and(|r| r.saves_plans()),
        reads_identifiers: plugin
            .runner
            .as_ref()
            .is_some_and(|r| r.reads_identifiers()),
        debug_levels: plugin
            .runner
            .as_ref()
            .map(|r| crate::debug::describe_all(r.debug_levels(), r.more_debug_levels()))
            .unwrap_or_default(),
        destroys: plugin.runner.as_ref().is_some_and(|r| r.destroys()),
        inspects: plugin.runner.as_ref().is_some_and(|r| r.inspects()),
        settings: plugin
            .runner
            .as_ref()
            .map(|r| r.settings())
            .unwrap_or_default(),
        importer: plugin.importer.clone(),
        exports_schemas: plugin.schemas.is_some(),
        protocol,
    });
    let mut router = tonic::transport::Server::builder().add_service(emitter);
    if let Some(source) = &plugin.schemas {
        router = router.add_service(SchemaSourceServer::new(SchemaSourceHandler {
            source: Arc::clone(source),
        }));
    }
    if let Some(runner) = &plugin.runner {
        router = router.add_service(RunnerServer::new(RunnerHandler {
            runner: Arc::clone(runner),
            active: Arc::new(Mutex::new(None)),
        }));
    }
    if let Some(importer) = &plugin.importer {
        router = router.add_service(ImporterServer::new(ImporterHandler {
            importer: Arc::clone(importer),
        }));
    }

    let Listening {
        incoming, _guard, ..
    } = listening;
    router
        .serve_with_incoming_shutdown(incoming, host_is_done())
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

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    // Windows has no termination signal a process can catch; closing stdin
    // and Ctrl-C are the two ways a plugin is told to stop there.
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = stdin_closed => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate => {}
    }
}

struct EmitterHandler {
    emitter: Arc<dyn Emitter + Send + Sync>,
    operations: Operations,
    accepts_inputs: bool,
    /// Whether the runner saves plans and applies exactly a saved one.
    saves_plans: bool,
    /// Whether the runner reads its tool's identifiers.
    reads_identifiers: bool,
    /// What each debug level does to the tool.
    debug_levels: Vec<String>,
    /// Whether the runner destroys what its tool created.
    destroys: bool,
    /// Whether the runner reads one resource as it is now.
    inspects: bool,
    /// What the runner can be configured with, for the host to list.
    settings: Vec<crate::Setting>,
    /// Kept only to describe it: importing is served by its own handler.
    importer: Option<Arc<dyn Importer>>,
    /// Whether a schema source is served, likewise by its own handler.
    exports_schemas: bool,
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
            placed_text: wire::placed_text_to_wire(&self.emitter.placed_text()),
            category: wire::category_to_wire(self.emitter.category()) as i32,
            target_versions: self.emitter.target_versions(),
            resource_schemas: self
                .emitter
                .resource_schemas()
                .iter()
                .map(wire::resource_schema_to_wire)
                .collect(),
            operations: Some(wire::operations_to_wire(self.operations)),
            accepts_inputs: self.accepts_inputs,
            saves_plans: self.saves_plans,
            reads_identifiers: self.reads_identifiers,
            debug_levels: self.debug_levels.clone(),
            destroys: self.destroys,
            inspects: self.inspects,
            import_formats: wire::import_formats_to_wire(
                self.importer
                    .as_ref()
                    .map(|i| i.formats())
                    .unwrap_or_default(),
            ),
            exports_schemas: self.exports_schemas,
            settings: wire::settings_to_wire(self.settings.clone()),
            settings_keys: self.emitter.settings_keys(),
            rules: wire::rules_to_wire(&self.emitter.rules()),
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
        let outcome = tokio::task::spawn_blocking(move || {
            crate::emit_checked(emitter.as_ref(), &graph, &data)
        })
        .await
        .map_err(|e| Status::internal(format!("the emitter panicked: {e}")))?;
        Ok(Response::new(wire::emit_response(outcome)))
    }
}

struct SchemaSourceHandler {
    source: Arc<dyn SchemaSource>,
}

/// Where a schema source's log goes: the plugin's own stderr, which the host
/// shows when the export fails and `meridian schema sync` relays as it runs.
struct StderrLog;

impl Log for StderrLog {
    fn line(&self, line: LogLine) {
        let prefix = match line.stream {
            LogStream::Stdout => "",
            LogStream::Stderr => "! ",
        };
        eprintln!("{prefix}{}", line.text);
    }
}

#[tonic::async_trait]
impl SchemaSourceService for SchemaSourceHandler {
    async fn export_schemas(
        &self,
        request: Request<pb::ExportSchemasRequest>,
    ) -> Result<Response<pb::ExportSchemasResponse>, Status> {
        let request = wire::export_schemas_request_from_wire(request.into_inner())
            .map_err(Status::invalid_argument)?;
        let source = Arc::clone(&self.source);
        // A source runs a tool and parses what it prints, so a panic in it
        // must reach the host as this call failing, not take the plugin
        // down.
        let outcome = tokio::task::spawn_blocking(move || source.export(&request, &StderrLog))
            .await
            .map_err(|e| Status::internal(format!("the schema source panicked: {e}")))?;
        Ok(Response::new(wire::export_schemas_response(outcome)))
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
    /// The stop flag of the plan or apply that is running, so that `Cancel`
    /// can reach it. A host drives one run of a plugin at a time.
    active: Active,
}

/// The stop flag of the run in progress, when there is one.
type Active = Arc<Mutex<Option<Arc<AtomicBool>>>>;

type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

/// Relays a runner's log lines into the response stream as they are written,
/// and tells the runner when the host has asked it to stop.
struct StreamLog<T> {
    events: mpsc::UnboundedSender<Result<T, Status>>,
    wrap: fn(pb::LogLine) -> T,
    stop: Arc<AtomicBool>,
}

impl<T: Send> Log for StreamLog<T> {
    fn line(&self, line: LogLine) {
        // The host hanging up mid-run is not the runner's problem: the tool
        // is already running and should be left to finish. A host that wants
        // the tool stopped says so with `Cancel`.
        let _ = self.events.send(Ok((self.wrap)(wire::log_to_wire(line))));
    }

    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// Runs a blocking runner call, streaming its log lines and then its result.
/// While it runs, its stop flag is the one `Cancel` sets.
fn run_streaming<T, R>(
    active: &Active,
    wrap_log: fn(pb::LogLine) -> T,
    wrap_result: fn(R) -> T,
    call: impl FnOnce(&dyn Log) -> R + Send + 'static,
) -> EventStream<T>
where
    T: Send + 'static,
    R: Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    *active.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&stop));
    let active = Arc::clone(active);
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
            stop: Arc::clone(&stop),
        };
        let outcome = tokio::task::spawn_blocking(move || call(&log)).await;
        // The run is over: a Cancel that arrives now has nothing to stop.
        {
            let mut running = active.lock().unwrap_or_else(|e| e.into_inner());
            if running
                .as_ref()
                .is_some_and(|flag| Arc::ptr_eq(flag, &stop))
            {
                *running = None;
            }
        }
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

fn destroy_log(line: pb::LogLine) -> pb::DestroyEvent {
    pb::DestroyEvent {
        event: Some(pb::destroy_event::Event::Log(line)),
    }
}

fn destroy_done(result: pb::DestroyResult) -> pb::DestroyEvent {
    pb::DestroyEvent {
        event: Some(pb::destroy_event::Event::Result(result)),
    }
}

#[tonic::async_trait]
impl RunnerService for RunnerHandler {
    type PlanStream = EventStream<pb::PlanEvent>;
    type ApplyStream = EventStream<pb::ApplyEvent>;
    type DestroyStream = EventStream<pb::DestroyEvent>;

    async fn plan(
        &self,
        request: Request<pb::PlanRequest>,
    ) -> Result<Response<Self::PlanStream>, Status> {
        if !self.runner.operations().plan {
            return Err(Status::unimplemented("this plugin does not plan"));
        }
        let request = request.into_inner();
        // A host must never get an apply's preview when it asked what a
        // destroy would remove.
        if request.destroy && !self.runner.destroys() {
            return Err(Status::unimplemented("this plugin does not destroy"));
        }
        let destroy = request.destroy;
        let context = wire::context_from_wire(request.context).map_err(Status::invalid_argument)?;
        let artifact = wire::artifact_from_wire(request.artifact);
        let runner = Arc::clone(&self.runner);
        Ok(Response::new(run_streaming(
            &self.active,
            plan_log,
            plan_done,
            move |log| {
                wire::plan_result(if destroy {
                    runner.plan_destroy(&context, &artifact, log)
                } else {
                    runner.plan(&context, &artifact, log)
                })
            },
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
            &self.active,
            apply_log,
            apply_done,
            move |log| wire::apply_result(runner.apply(&context, &artifact, log)),
        )))
    }

    async fn destroy(
        &self,
        request: Request<pb::DestroyRequest>,
    ) -> Result<Response<Self::DestroyStream>, Status> {
        if !self.runner.destroys() {
            return Err(Status::unimplemented("this plugin does not destroy"));
        }
        let request = request.into_inner();
        let context = wire::context_from_wire(request.context).map_err(Status::invalid_argument)?;
        let artifact = wire::artifact_from_wire(request.artifact);
        let runner = Arc::clone(&self.runner);
        Ok(Response::new(run_streaming(
            &self.active,
            destroy_log,
            destroy_done,
            move |log| wire::destroy_result(runner.destroy(&context, &artifact, log)),
        )))
    }

    async fn cancel(
        &self,
        _request: Request<pb::CancelRequest>,
    ) -> Result<Response<pb::CancelResponse>, Status> {
        let was_running = match self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            Some(stop) => {
                stop.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        };
        Ok(Response::new(pb::CancelResponse { was_running }))
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

    async fn identifiers(
        &self,
        request: Request<pb::IdentifiersRequest>,
    ) -> Result<Response<pb::IdentifiersResponse>, Status> {
        if !self.runner.reads_identifiers() {
            return Err(Status::unimplemented(
                "this plugin does not read identifiers",
            ));
        }
        let context = wire::context_from_wire(request.into_inner().context)
            .map_err(Status::invalid_argument)?;
        let runner = Arc::clone(&self.runner);
        let outcome = tokio::task::spawn_blocking(move || runner.identifiers(&context))
            .await
            .map_err(|e| Status::internal(format!("the runner panicked: {e}")))?;
        Ok(Response::new(wire::identifiers_response(outcome)))
    }

    async fn inspect(
        &self,
        request: Request<pb::InspectRequest>,
    ) -> Result<Response<pb::InspectResponse>, Status> {
        if !self.runner.inspects() {
            return Err(Status::unimplemented("this plugin does not inspect"));
        }
        let request = request.into_inner();
        let context = wire::context_from_wire(request.context).map_err(Status::invalid_argument)?;
        let artifact = wire::artifact_from_wire(request.artifact);
        let runner = Arc::clone(&self.runner);
        let (resource, endpoint) = (request.resource, request.endpoint);
        let outcome = tokio::task::spawn_blocking(move || {
            runner.inspect(&context, &artifact, &resource, &endpoint)
        })
        .await
        .map_err(|e| Status::internal(format!("the runner panicked: {e}")))?;
        Ok(Response::new(wire::inspect_response(outcome)))
    }
}
