//! The `meridian` command.
//!
//! It is a thin shell: it reads documents, loads plugins, and prints what the
//! orchestrator reports. Stdout carries results, stderr carries the tools'
//! own output and errors, and the exit status says whether everything asked
//! for was done.

mod tools;

use std::collections::BTreeMap;
mod import;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use meridian_ir::Bundle;
use meridian_orchestrator::{
    lock, record, run_selected, selected, Backend, Drift, Event, ExecBackend, LocalBackend, Mode,
    Observer, OnFailure, Options, Outcome, Policy, Resume, RollbackOutcome, RollbackReport,
    RunRecord, Selector, StageId, StageReport, MASK, REASON_POLICY_DENIED,
};
use meridian_plugin_host::{
    signature::signature_path, Policy as SignaturePolicy, PublicKey, Registry, SecretKey,
    Signature, StartOptions, Verification,
};
use meridian_sdk::{ChangeAction, Data, LogLine, LogStream, Output};

#[derive(Parser)]
#[command(
    name = "meridian",
    version,
    about = "One document, any tool: compile Meridian documents and run the tools they target"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[command(flatten)]
    trust: Trust,
}

/// Whose signatures count, for plugins and for modules alike, and whether
/// modules must carry one. These can be given to any command.
#[derive(Args)]
struct Trust {
    /// What to do about module signatures: off (do not look), warn (use an
    /// unsigned or wrongly signed module, and say so) or require (use only
    /// modules signed by a trusted key). Applies to modules fetched from git
    /// or a registry, when they are installed and every time they are used
    #[arg(
        long = "module-signatures",
        global = true,
        value_name = "POLICY",
        env = "MERIDIAN_MODULE_SIGNATURES",
        default_value = "off"
    )]
    module_signatures: SignaturePolicy,
    #[command(flatten)]
    keys: TrustedKeys,
}

/// The trusted keys the command line named, kept for the whole run: plugins
/// and modules are checked against the same ones.
static TRUSTED_KEYS: OnceLock<TrustedKeys> = OnceLock::new();
/// The policy and keys fetched modules are checked against.
static MODULE_SIGNATURES: OnceLock<meridian_module::Verification> = OnceLock::new();

/// Every key to trust: the release's own, then the ones named.
fn trusted_keys() -> Result<Vec<PublicKey>, Failure> {
    match TRUSTED_KEYS.get() {
        Some(keys) => keys.read(),
        None => TrustedKeys::default().read(),
    }
}

/// Settles the trust settings once, before any command runs.
fn settle(trust: Trust) -> Result<(), Failure> {
    let policy = trust.module_signatures;
    let _ = TRUSTED_KEYS.set(trust.keys);
    // With signatures off the keys are not needed, and a key file that has
    // gone missing should not stop a run that does not use it.
    let trusted = match policy {
        SignaturePolicy::Off => Vec::new(),
        SignaturePolicy::Warn | SignaturePolicy::Require => trusted_keys()?,
    };
    let _ = MODULE_SIGNATURES.set(meridian_module::Verification { policy, trusted });
    Ok(())
}

#[derive(Subcommand)]
enum Command {
    /// Check documents for errors without loading any plugin
    Validate {
        /// The documents to check
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Where fetched modules are kept, for a file written @module:path
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
    },
    /// Compile each document into its target's own files, and change nothing
    Compile {
        #[command(flatten)]
        workflow: Workflow,
        /// Write the compiled files here, one directory per document
        #[arg(short, long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Show what applying the documents would change
    Plan {
        #[command(flatten)]
        workflow: Workflow,
    },
    /// Make the documents' desired state real
    Apply {
        #[command(flatten)]
        workflow: Workflow,
        /// Continue an apply that failed or was interrupted: stages it
        /// already applied, from documents that have not changed since, are
        /// left alone. With no value, the last apply; or a run id
        #[arg(long, value_name = "RUN", num_args = 0..=1, default_missing_value = "last")]
        resume: Option<String>,
        /// What to do when a stage fails: stop, leaving what was applied in
        /// place, or rollback, putting every stage this run changed back to
        /// what was applied before
        #[arg(long, value_name = "ACTION", default_value = "stop")]
        on_failure: FailureAction,
    },
    /// Check whether what was applied is still what is there, changing
    /// nothing; exits 2 when something has drifted
    Drift {
        #[command(flatten)]
        workflow: Workflow,
    },
    /// Put back what the last apply changed: each stage it applied is
    /// applied again from what was applied before it, last stage first
    Rollback {
        #[command(flatten)]
        workflow: Workflow,
        /// The apply to put back; the last one when not given
        #[arg(long, value_name = "RUN")]
        run: Option<String>,
    },
    /// Show the current or last run, step by step, and who holds the lock
    Status {
        #[command(flatten)]
        state: State,
    },
    /// List the runs recorded in the state, oldest first
    Runs {
        #[command(flatten)]
        state: State,
    },
    /// Print a run's events, one JSON object per line
    Events {
        /// The run; the last one when not given
        run: Option<String>,
        /// Only events of this type, such as step.failed; step.* takes a
        /// whole family; may be given more than once
        #[arg(long = "type", value_name = "TYPE")]
        types: Vec<String>,
        #[command(flatten)]
        state: State,
    },
    /// Remove the state lock a run left behind when it was killed
    Unlock {
        /// The lock's id, as `meridian status` and the refused run show it
        id: String,
        #[command(flatten)]
        state: State,
    },
    /// Turn a tool's existing configuration into Meridian documents
    ///
    /// Reads the files, or the files in the directories, and writes one
    /// document for each thing the plugin finds in them. Whatever cannot be
    /// carried over is reported and marked in the documents with
    /// TODO(meridian-import); nothing is dropped quietly. Nothing is run and
    /// nothing is changed: importing only reads.
    Import {
        /// The files to import, or directories holding them
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// The tool the files belong to, as the name of its plugin, such as
        /// terraform or ansible. Left out, it is worked out from the file
        /// names when only one plugin claims them
        #[arg(long, value_name = "TARGET")]
        from: Option<String>,
        /// What to call the imported document. Defaults to the name of the
        /// directory or file
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Write the documents into this directory, one file each. Left out,
        /// they are printed
        #[arg(short, long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Look in the directories inside a directory too
        #[arg(short, long)]
        recursive: bool,
        /// Replace a document that already exists in the output directory
        #[arg(long)]
        force: bool,
        /// Exit with an error when anything was not carried over as written.
        /// The documents are still written
        #[arg(long)]
        strict: bool,
        #[command(flatten)]
        plugins: PluginDirs,
    },
    /// List the plugins Meridian can find and what each can do
    Plugins {
        #[command(flatten)]
        plugins: PluginDirs,
    },
    /// Work with compile rules: the policies layered data must keep
    Rules {
        #[command(subcommand)]
        command: RulesCommand,
    },
    /// Rewrite documents into a newer language version (apiVersion)
    Migrate {
        /// The files to rewrite, in place
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// The language version to move to, such as meridian/v1. Defaults to
        /// the newest this build supports
        #[arg(long, value_name = "APIVERSION")]
        to: Option<String>,
        /// Change nothing; exit non-zero if any file would change
        #[arg(long)]
        check: bool,
    },
    /// Show every version this build carries: core, language, SDK and
    /// plugin protocol
    Version,
    /// Work with the secrets documents declare, without showing any
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    /// Rewrite documents in the one standard layout, changing no value
    Fmt(tools::FmtArgs),
    /// Report errors, and things that compile but are probably mistakes
    Lint(tools::LintArgs),
    /// Print a JSON Schema of the document language, for YAML editors
    Schema(tools::SchemaArgs),
    /// Run the language server, for editors, on standard input and output
    Lsp(tools::LspArgs),
    /// Make signing keys, sign plugin binaries and check their signatures
    Signing {
        #[command(subcommand)]
        command: SigningCommand,
    },
    /// Fetch, pin and check the modules the project's meridian.yaml requires
    Module {
        #[command(subcommand)]
        command: ModuleCommand,
        /// Where fetched modules are kept
        #[arg(
            long,
            global = true,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum ModuleCommand {
    /// Fetch what meridian.yaml requires and write meridian.lock
    Install {
        /// Fail if meridian.lock would have to change; for CI
        #[arg(long)]
        locked: bool,
    },
    /// Move pinned modules to the newest version that still fits
    Update {
        /// The modules to move; every module when none is named
        names: Vec<String>,
    },
    /// Check every pinned module against the hash in meridian.lock, and its
    /// signature when --module-signatures says to
    Verify,
    /// List the pinned modules, their versions and where they come from
    List,
    /// Sign this module as it is now, writing meridian.sig in it. Do it
    /// last, before the version is tagged or pushed
    Sign {
        /// The secret key to sign with, from `meridian signing keygen`
        #[arg(long, value_name = "FILE")]
        key: PathBuf,
    },
    /// Write this module as the archive a registry stores
    Pack {
        /// The file to write. Defaults to a file under the state directory
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Publish this module to an OCI registry, under its version
    Push {
        /// The repository to publish to, such as
        /// registry.example.com/team/module, with no scheme and no tag
        repository: String,
        /// Talk plain HTTP. For a registry on a trusted network only
        #[arg(long)]
        insecure: bool,
        /// Log in with this credential from spec.credentials in
        /// meridian.yaml, instead of a login from the Docker configuration
        #[arg(long, value_name = "NAME")]
        credential: Option<String>,
    },
}

#[derive(Subcommand)]
enum RulesCommand {
    /// Load a hierarchy's layers and check them against its compile rules,
    /// compiling nothing
    Check {
        /// The hierarchy file; its layers are found relative to it
        #[arg(long, value_name = "FILE")]
        hierarchy: PathBuf,
        /// The rules file. Defaults to rules.yaml beside the hierarchy file
        #[arg(long, value_name = "FILE")]
        rules: Option<PathBuf>,
        /// Fail when there is no rules file, so that removing it cannot
        /// switch the rules off unnoticed
        #[arg(long)]
        require_rules: bool,
        /// One value, as on compile: merged over the hierarchy last and
        /// checked as its most specific layer. The hierarchy's layer paths
        /// also read it as %{KEY}; may be given more than once
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// A YAML or JSON file of values merged over the hierarchy, checked
        /// as its most specific layer; may be given more than once
        #[arg(long, value_name = "FILE")]
        data: Vec<PathBuf>,
        /// Where fetched modules are kept, for a --data file named
        /// @<module>:<path>
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum SigningCommand {
    /// Make a new key pair for signing plugins
    Keygen {
        /// Where to write the secret key, which signs. Keep it private
        #[arg(long, value_name = "FILE")]
        secret: PathBuf,
        /// Where to write the public key, which those who run the plugins trust
        #[arg(long, value_name = "FILE")]
        public: PathBuf,
    },
    /// Sign plugin binaries, writing <BINARY>.sig beside each
    Sign {
        /// The secret key to sign with
        #[arg(long, value_name = "FILE")]
        key: PathBuf,
        /// The binaries to sign
        #[arg(required = true)]
        binaries: Vec<PathBuf>,
    },
    /// Check the signatures of plugin binaries against the trusted keys
    Verify {
        /// The binaries to check
        #[arg(required = true)]
        binaries: Vec<PathBuf>,
    },
}

/// The public key this build's own plugins were signed with, when it is a
/// release build: the release workflow sets the variable while it compiles.
/// A build from source has none.
const RELEASE_PUBLIC_KEY: Option<&str> = option_env!("MERIDIAN_RELEASE_PUBLIC_KEY");

#[derive(Args, Default)]
struct TrustedKeys {
    /// A public key file whose signatures on plugins and modules are
    /// trusted, or a directory of *.pub files; may be given more than once.
    /// A released meridian already trusts the key its own plugins were
    /// signed with
    #[arg(
        long = "trusted-key",
        global = true,
        value_name = "FILE",
        env = "MERIDIAN_TRUSTED_KEYS",
        value_delimiter = PATH_LIST_SEPARATOR
    )]
    trusted_keys: Vec<PathBuf>,
}

impl TrustedKeys {
    /// Every key to trust: the release's own, then the ones named.
    fn read(&self) -> Result<Vec<PublicKey>, Failure> {
        let mut keys = Vec::new();
        if let Some(key) = RELEASE_PUBLIC_KEY.filter(|k| !k.trim().is_empty()) {
            keys.push(PublicKey::parse(key).map_err(|e| {
                Failure::Message(format!("the public key built into this meridian: {e}"))
            })?);
        }
        for path in &self.trusted_keys {
            if path.is_dir() {
                let mut files: Vec<PathBuf> = std::fs::read_dir(path)
                    .map_err(|e| {
                        Failure::Message(format!(
                            "reading the key directory {}: {e}",
                            path.display()
                        ))
                    })?
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|file| file.extension().is_some_and(|ext| ext == "pub"))
                    .collect();
                files.sort();
                for file in files {
                    keys.push(PublicKey::read(&file).map_err(Failure::Message)?);
                }
            } else {
                keys.push(PublicKey::read(path).map_err(Failure::Message)?);
            }
        }
        Ok(keys)
    }
}

/// What separates the directories in `MERIDIAN_PLUGIN_DIR`: the system's own
/// separator for a list of paths, since a Windows path holds a colon.
const PATH_LIST_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

#[derive(Args)]
pub(crate) struct PluginDirs {
    /// A directory to look for plugins in; may be given more than once.
    /// Defaults to the directory the meridian binary is in
    #[arg(
        long = "plugin-dir",
        value_name = "DIR",
        env = "MERIDIAN_PLUGIN_DIR",
        value_delimiter = PATH_LIST_SEPARATOR
    )]
    pub(crate) dirs: Vec<PathBuf>,
    /// What to do about plugin signatures: off (do not look), warn (start an
    /// unsigned or wrongly signed plugin, and say so) or require (start only
    /// plugins signed by a trusted key). Defaults to require in a released
    /// meridian and to off in one built from source
    #[arg(
        long = "plugin-signatures",
        value_name = "POLICY",
        env = "MERIDIAN_PLUGIN_SIGNATURES"
    )]
    signatures: Option<SignaturePolicy>,
}

impl PluginDirs {
    fn verification(&self) -> Result<Verification, Failure> {
        let policy = self.signatures.unwrap_or(match RELEASE_PUBLIC_KEY {
            Some(key) if !key.trim().is_empty() => SignaturePolicy::Require,
            _ => SignaturePolicy::Off,
        });
        // With signatures off the keys are not needed, and a key file that
        // has gone missing should not stop a run that does not use it.
        let trusted = match policy {
            SignaturePolicy::Off => Vec::new(),
            SignaturePolicy::Warn | SignaturePolicy::Require => trusted_keys()?,
        };
        Ok(Verification { policy, trusted })
    }
}

/// Where Meridian keeps what it knows.
#[derive(Args)]
struct State {
    #[command(flatten)]
    plugins: PluginDirs,
    /// Where each tool's working files are kept, and with the local backend
    /// Meridian's own record: the run-state, the runs and the lock
    #[arg(
        long,
        value_name = "DIR",
        env = "MERIDIAN_STATE_DIR",
        default_value = ".meridian"
    )]
    state_dir: PathBuf,
    /// Where Meridian's own record is kept: `local`, the state directory,
    /// or the name of a backend program, meridian-backend-<NAME>, in a
    /// plugin directory
    #[arg(
        long,
        value_name = "NAME",
        env = "MERIDIAN_BACKEND",
        default_value = "local"
    )]
    backend: String,
    /// A setting for the backend, as key=value; may be given more than
    /// once. The backend program reads it as MERIDIAN_BACKEND_<KEY>
    #[arg(long, value_name = "KEY=VALUE")]
    backend_config: Vec<String>,
}

/// What an apply does when a stage fails.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FailureAction {
    /// Stop, and leave what was applied in place
    Stop,
    /// Stop, then put back every stage this run changed
    Rollback,
}

/// How the events of a run are written to standard output.
#[derive(Clone, Copy, ValueEnum)]
enum EventFormat {
    /// One JSON object per line
    Json,
}

#[derive(Args)]
struct Workflow {
    /// The documents that make up the workflow
    #[arg(required = true)]
    files: Vec<PathBuf>,
    #[command(flatten)]
    state: State,
    /// How long to wait for another run to release the state lock, as in
    /// 30s, 5m or 1h. By default a locked state is refused at once
    #[arg(long, value_name = "DURATION", default_value = "0s", value_parser = parse_duration)]
    lock_timeout: Duration,
    /// Write every event of the run to standard output as it happens, one
    /// JSON object per line. The report for people moves to standard error
    #[arg(long, value_name = "FORMAT")]
    events: Option<EventFormat>,
    /// How many runs' records to keep; older ones are removed when a run ends
    #[arg(long, value_name = "N", default_value_t = meridian_orchestrator::DEFAULT_KEEP_RUNS)]
    keep_runs: usize,
    /// Run only the documents whose metadata.labels match, as in
    /// `tier=web,env!=prod`; may be given more than once, and every
    /// requirement must hold. Forms: key=value, key!=value, key, !key
    #[arg(short = 'l', long, value_name = "SELECTOR")]
    selector: Vec<String>,
    /// A YAML or JSON file of values that `when` conditions read as
    /// data.<key>; may be given more than once, and a later file's keys
    /// replace an earlier file's
    #[arg(long, value_name = "FILE")]
    data: Vec<PathBuf>,
    /// A policy file or directory that plan and apply check every stage
    /// against before anything is applied; may be given more than once. A
    /// change the policy denies, or a policy that cannot be asked, fails
    /// the run
    #[arg(long, value_name = "PATH")]
    policy: Vec<PathBuf>,
    /// The engine that reads the policies: the program
    /// meridian-policy-<NAME> in a plugin directory
    #[arg(long, value_name = "NAME", env = "MERIDIAN_POLICY_ENGINE", default_value = meridian_orchestrator::policy::DEFAULT_ENGINE)]
    policy_engine: String,
    /// One value for `when` conditions, as in `--set env=prod` or
    /// `--set region.name=eu-west-1`; may be given more than once. It is
    /// applied after every --data file, so it wins. The value is typed as
    /// YAML would type it: true, 3 and null are not text, "3" is
    #[arg(long, value_name = "KEY=VALUE")]
    set: Vec<String>,
    /// Refuse to run when a secret cannot be locked in memory, instead of
    /// warning once and going on
    #[arg(long, env = "MERIDIAN_STRICT_SECRETS")]
    strict_secrets: bool,
}

#[derive(Subcommand)]
enum SecretCommand {
    /// Say, for every secret the documents declare, where it comes from and
    /// whether that place would answer, without reading any value
    Check {
        /// The documents whose secrets to check
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Where fetched modules are kept, for a file written @module:path
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
    },
}

fn main() -> ExitCode {
    // Before anything is read: no core dump of this process, and no other
    // process of the same user reading its memory, so a secret fetched later
    // cannot leave that way.
    let _ = meridian_secret::harden_process();
    let cli = Cli::parse();
    let outcome = settle(cli.trust).and_then(|()| match cli.command {
        Command::Validate { files, state_dir } => load(&files, &state_dir).map(|bundle| {
            println!("{} valid", count(bundle.documents.len(), "document"));
        }),
        Command::Module { command, state_dir } => module(command, &state_dir),
        Command::Secret {
            command: SecretCommand::Check { files, state_dir },
        } => check_secrets(&files, &state_dir),
        Command::Compile { workflow, out } => orchestrate(
            &workflow,
            Mode::Compile,
            &How {
                out: out.as_deref(),
                ..How::default()
            },
        ),
        Command::Plan { workflow } => orchestrate(&workflow, Mode::Plan, &How::default()),
        Command::Apply {
            workflow,
            resume,
            on_failure,
        } => orchestrate(
            &workflow,
            Mode::Apply,
            &How {
                resume: resume.as_deref(),
                rollback_on_failure: on_failure == FailureAction::Rollback,
                ..How::default()
            },
        ),
        Command::Drift { workflow } => orchestrate(&workflow, Mode::Drift, &How::default()),
        Command::Rollback { workflow, run } => orchestrate(
            &workflow,
            Mode::Rollback,
            &How {
                rollback_of: run.as_deref(),
                ..How::default()
            },
        ),
        Command::Status { state } => status(&state),
        Command::Runs { state } => runs(&state),
        Command::Events { run, types, state } => events(&state, run.as_deref(), &types),
        Command::Unlock { id, state } => unlock(&state, &id),
        Command::Import {
            paths,
            from,
            name,
            out,
            recursive,
            force,
            strict,
            plugins,
        } => import_configuration(
            &plugins,
            &import::Request {
                paths: &paths,
                from: from.as_deref(),
                name: name.as_deref(),
                out: out.as_deref(),
                recursive,
                force,
                strict,
            },
        ),
        Command::Plugins { plugins } => list_plugins(&plugins),
        Command::Rules {
            command:
                RulesCommand::Check {
                    hierarchy,
                    rules,
                    require_rules,
                    set,
                    data,
                    state_dir,
                },
        } => check_rules(
            meridian_rules::Files {
                hierarchy,
                rules,
                require_rules,
            },
            &set,
            &data,
            &state_dir,
        ),
        Command::Migrate { files, to, check } => migrate(&files, to.as_deref(), check),
        Command::Version => {
            print!("{}", versions());
            Ok(())
        }
        Command::Fmt(args) => tools::format(&args),
        Command::Lint(args) => tools::lint(&args),
        Command::Schema(args) => tools::schema(&args),
        Command::Lsp(args) => tools::lsp(&args),
        Command::Signing { command } => signing(command),
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Reported) => ExitCode::FAILURE,
        Err(Failure::Drifted) => ExitCode::from(2),
        Err(Failure::Message(msg)) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) enum Failure {
    /// Something to print.
    Message(String),
    /// Already printed, stage by stage.
    Reported,
    /// A drift check ran, and found drift. Already printed.
    Drifted,
}

pub(crate) fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Reads and validates the documents. Every problem is reported, each at its
/// own line, before anything else happens.
fn load(files: &[PathBuf], state_dir: &Path) -> Result<Bundle, Failure> {
    let problems = |errors: meridian_ir::ErrorList| {
        eprintln!("error: {errors}");
        Failure::Reported
    };
    let files = in_modules(files, state_dir)?;
    let bundle = meridian_ir::load(&files).map_err(problems)?;
    bundle.validate().map_err(problems)?;
    // Once per run, however many documents: see the versioning policy.
    if let Some(warning) = bundle.deprecation_warning() {
        eprintln!("warning: {warning}");
    }
    Ok(bundle)
}

/// Every version stream of this build, one per line.
fn versions() -> String {
    use meridian_ir::Status;
    use meridian_sdk::version::{describe_protocols, supported_protocols};
    let mut out = format!("meridian {}\n", env!("CARGO_PKG_VERSION"));
    for v in meridian_ir::VersionTable::builtin().versions() {
        let status = match v.status {
            Status::Supported => "supported".to_owned(),
            Status::Deprecated { since } => format!("deprecated since meridian {since}"),
            Status::Removed { in_release } => format!("removed in meridian {in_release}"),
        };
        out.push_str(&format!("  language         {}  {status}\n", v.name()));
    }
    out.push_str(&format!("  plugin sdk       {}\n", meridian_sdk::VERSION));
    out.push_str(&format!(
        "  plugin protocol  {}\n",
        describe_protocols(&supported_protocols())
    ));
    out.push_str(&format!(
        "  lockfile format  {}\n",
        meridian_module::LOCK_VERSION
    ));
    out
}

/// Rewrites each file into the language version asked for. A file is either
/// rewritten whole or left exactly as it was.
fn migrate(files: &[PathBuf], to: Option<&str>, check: bool) -> Result<(), Failure> {
    use meridian_ir::{MigrateError, Migrated, VersionTable};
    let table = VersionTable::builtin();
    let target = match to {
        Some(text) => table
            .check(text)
            .map_err(|e| Failure::Message(format!("--to: {e}")))?,
        None => table
            .current()
            .ok_or_else(|| Failure::Message("this build supports no language version".into()))?,
    };
    let mut failed = false;
    let mut would_change = 0usize;
    for file in files {
        let name = file.display();
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("error: {name}: cannot read file: {e}");
                failed = true;
                continue;
            }
        };
        match meridian_ir::migrate(&table, meridian_ir::STEPS, &text, target.number) {
            Ok(Migrated::Unchanged) => println!("{name}: already at {}", target.name()),
            Ok(Migrated::Rewritten(_)) if check => {
                would_change += 1;
                println!("{name}: would be migrated to {}", target.name());
            }
            Ok(Migrated::Rewritten(new)) => match replace_file(file, &new) {
                Ok(()) => println!("{name}: migrated to {}", target.name()),
                Err(e) => {
                    eprintln!("error: {name}: cannot write file: {e}");
                    failed = true;
                }
            },
            Err(MigrateError::Refused(why)) => {
                eprintln!("error: {name}: {why}");
                failed = true;
            }
            Err(MigrateError::NeedsDecisions(decisions)) => {
                eprintln!(
                    "error: {name}: not migrated, because it needs {} only you can make:",
                    count(decisions.len(), "decision")
                );
                for decision in decisions {
                    eprintln!("  {decision}");
                }
                failed = true;
            }
        }
    }
    if failed {
        return Err(Failure::Reported);
    }
    if would_change > 0 {
        return Err(Failure::Message(format!(
            "{} would change; run without --check to migrate",
            count(would_change, "file")
        )));
    }
    Ok(())
}

/// Replaces a file's content by writing beside it and renaming, so a crash
/// leaves the old file or the new one and never half of either.
fn replace_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(".meridian-tmp");
    let staged = PathBuf::from(staged);
    std::fs::write(&staged, content)?;
    std::fs::rename(&staged, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

/// The project in the current directory, as the module commands see it.
fn project(state_dir: &Path) -> meridian_module::Project<'_> {
    meridian_module::Project {
        dir: Path::new("."),
        state_dir,
        core_version: env!("CARGO_PKG_VERSION"),
        signatures: MODULE_SIGNATURES
            .get()
            .unwrap_or(&meridian_module::NO_SIGNATURES),
    }
}

/// Says, once a run, which modules are in use although their signature did
/// not pass. Only the warn policy lets that happen.
fn warn_about_modules(warnings: &[String]) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if !warnings.is_empty() && !SAID.swap(true, Ordering::Relaxed) {
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
    }
}

fn module_failure(e: meridian_module::Error) -> Failure {
    Failure::Message(e.to_string())
}

/// Turns each file written `@<module>:<path>` into the file it names in a
/// pinned module. Nothing is fetched: a module that is not installed, or
/// does not match its pin, stops the command here.
fn in_modules(files: &[PathBuf], state_dir: &Path) -> Result<Vec<PathBuf>, Failure> {
    use meridian_module::Modules;
    let is_reference = |file: &PathBuf| file.to_str().is_some_and(Modules::is_reference);
    if !files.iter().any(is_reference) {
        return Ok(files.to_vec());
    }
    let modules = Modules::load(&project(state_dir)).map_err(module_failure)?;
    warn_about_modules(modules.warnings());
    files
        .iter()
        .map(|file| match file.to_str() {
            Some(text) if Modules::is_reference(text) => {
                modules.resolve(text).map_err(module_failure)
            }
            _ => Ok(file.clone()),
        })
        .collect()
}

fn module(command: ModuleCommand, state_dir: &Path) -> Result<(), Failure> {
    use meridian_module::{Change, Mode, LOCK_FILE};
    let project = project(state_dir);
    let named = |m: &meridian_module::Locked| {
        if m.version.is_empty() {
            m.name.clone()
        } else {
            format!("{} {}", m.name, m.version)
        }
    };
    let mode = match command {
        ModuleCommand::Install { locked: true } => Mode::Locked,
        ModuleCommand::Install { locked: false } => Mode::Install,
        ModuleCommand::Update { names } => Mode::Update(names),
        ModuleCommand::Verify => {
            let checks = meridian_module::verify(&project).map_err(module_failure)?;
            if checks.is_empty() {
                println!("no modules are pinned");
            }
            let mut failed = 0;
            for check in &checks {
                match &check.problem {
                    None => match &check.signature {
                        Signature::NotChecked => println!("ok      {}", named(&check.module)),
                        Signature::Verified { key_id } => {
                            println!("ok      {}  signed by key {key_id}", named(&check.module))
                        }
                        Signature::Unsigned => {
                            println!("ok      {}  not signed", named(&check.module))
                        }
                        Signature::Invalid(why) => println!(
                            "ok      {}  signature does not pass: {why}",
                            named(&check.module)
                        ),
                    },
                    Some(problem) => {
                        failed += 1;
                        println!("FAILED  {}", named(&check.module));
                        for line in problem.lines() {
                            println!("    {line}");
                        }
                    }
                }
            }
            return if failed == 0 {
                Ok(())
            } else {
                Err(Failure::Message(format!(
                    "{} failed verification",
                    count(failed, "module")
                )))
            };
        }
        ModuleCommand::List => {
            let lock = meridian_module::Lock::load(project.dir)
                .map_err(module_failure)?
                .unwrap_or_default();
            if lock.modules.is_empty() {
                println!("no modules are pinned");
            }
            let width = |f: fn(&meridian_module::Locked) -> usize| {
                lock.modules.iter().map(f).max().unwrap_or(0)
            };
            let (names, versions) = (width(|m| m.name.len()), width(|m| m.version.len()));
            for m in &lock.modules {
                let version = if m.version.is_empty() {
                    "-"
                } else {
                    &m.version
                };
                println!("{:<names$}  {version:<versions$}  {}", m.name, m.source);
            }
            return Ok(());
        }
        ModuleCommand::Sign { key } => {
            let key = SecretKey::read(&key).map_err(Failure::Message)?;
            let signed = meridian_module::sign(project.dir, &key).map_err(module_failure)?;
            println!(
                "signed {}{} with key {}",
                signed.name,
                if signed.version.is_empty() {
                    String::new()
                } else {
                    format!(" {}", signed.version)
                },
                signed.key_id
            );
            println!("  integrity: {}", signed.integrity);
            println!("  wrote:     {}", meridian_module::SIGNATURE_FILE);
            return Ok(());
        }
        ModuleCommand::Pack { out } => return pack_module(&project, out),
        ModuleCommand::Push {
            repository,
            insecure,
            credential,
        } => return push_module(&project, &repository, insecure, credential.as_deref()),
    };

    let report = meridian_module::install(&project, mode).map_err(module_failure)?;
    warn_about_modules(&report.warnings());
    let mut unchanged = 0;
    for (m, change) in &report.changes {
        match change {
            Change::Added => println!("+ {} ({})", named(m), m.source),
            Change::Updated { from } if *from != m.version && !m.version.is_empty() => {
                println!("~ {} {from} -> {}", m.name, m.version)
            }
            Change::Updated { from } => println!("~ {} (was {from})", named(m)),
            Change::Removed => println!("- {}", named(m)),
            Change::Unchanged => unchanged += 1,
        }
    }
    if unchanged > 0 {
        println!("{} unchanged", count(unchanged, "module"));
    }
    if report.wrote_lock {
        println!("wrote {LOCK_FILE}");
    } else if report.lock.modules.is_empty() {
        println!("the project requires no modules");
    } else {
        println!("{LOCK_FILE} is up to date");
    }
    Ok(())
}

fn pack_module(
    project: &meridian_module::Project<'_>,
    out: Option<PathBuf>,
) -> Result<(), Failure> {
    let manifest = meridian_module::Manifest::load(project.dir).map_err(module_failure)?;
    let Some(version) = &manifest.version else {
        return Err(Failure::Message(format!(
            "{} has no metadata.version, and a module is published as a version",
            manifest.name
        )));
    };
    let archive = meridian_module::pack(project.dir).map_err(module_failure)?;
    let integrity = meridian_module::hash_dir(project.dir).map_err(module_failure)?;
    let out = out.unwrap_or_else(|| {
        project.state_dir.join("packages").join(format!(
            "{}-{version}.tar.gz",
            manifest.name.replace('/', "-")
        ))
    });
    let written = (|| {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, &archive)
    })();
    if let Err(e) = written {
        return Err(Failure::Message(format!("writing {}: {e}", out.display())));
    }
    println!("packed {} {version} into {}", manifest.name, out.display());
    println!("  size:       {} bytes", archive.len());
    println!("  integrity:  {integrity}");
    println!("  media type: {}", meridian_module::LAYER_MEDIA_TYPE);
    Ok(())
}

fn push_module(
    project: &meridian_module::Project<'_>,
    repository: &str,
    insecure: bool,
    credential: Option<&str>,
) -> Result<(), Failure> {
    let manifest = meridian_module::Manifest::load(project.dir).map_err(module_failure)?;
    let login = credential
        .map(|name| meridian_module::resolve_login(&manifest, name))
        .transpose()
        .map_err(module_failure)?;
    let Some(version) = &manifest.version else {
        return Err(Failure::Message(format!(
            "{} has no metadata.version, and a module is published as a version",
            manifest.name
        )));
    };
    let archive = meridian_module::pack(project.dir).map_err(module_failure)?;
    let pushed = meridian_module::push(
        repository,
        insecure,
        login.as_ref(),
        &manifest.name,
        version,
        &manifest.description,
        &archive,
    )
    .map_err(module_failure)?;
    if pushed.already_published {
        println!(
            "{} {version} is already published in {repository}, with this content",
            manifest.name
        );
    } else {
        println!(
            "published {} {version} to {repository}:{}",
            manifest.name, pushed.tag
        );
    }
    println!("  digest:     {}", pushed.digest);
    let integrity = meridian_module::hash_dir(project.dir).map_err(module_failure)?;
    println!("  integrity:  {integrity}");
    Ok(())
}

/// Reads the `--data` files into one map. Each file is a mapping; a key in a
/// later file replaces the same key from an earlier one, whole.
pub(crate) fn read_data(files: &[PathBuf], state_dir: &Path) -> Result<Data, Failure> {
    let mut data = Data::new();
    for file in &in_modules(files, state_dir)? {
        let name = file.to_string_lossy();
        let raw = std::fs::read(file)
            .map_err(|e| Failure::Message(format!("reading the data file {name}: {e}")))?;
        match meridian_ir::parse_value(&name, &raw) {
            Ok(meridian_ir::Value::Map(map)) => data.extend(map),
            Ok(meridian_ir::Value::Null) => {}
            Ok(other) => {
                return Err(Failure::Message(format!(
                    "the data file {name} must hold a mapping of names to values, and it \
                     holds {}",
                    other.describe()
                )))
            }
            Err(e) => return Err(Failure::Message(e.to_string())),
        }
    }
    Ok(data)
}

/// A length of time as a person writes one: `90`, `30s`, `5m`, `1h`,
/// `250ms`. A bare number is seconds.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let (number, unit) = text.split_at(digits);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("{text:?} is not a duration; write one as 30s, 5m or 1h"))?;
    match unit {
        "ms" => Ok(Duration::from_millis(number)),
        "" | "s" => Ok(Duration::from_secs(number)),
        "m" => Ok(Duration::from_secs(number.saturating_mul(60))),
        "h" => Ok(Duration::from_secs(number.saturating_mul(3600))),
        other => Err(format!(
            "{other:?} is not a unit of time; use ms, s, m or h"
        )),
    }
}

/// `meridian rules check`: loads the layers, checks every rule, and says
/// which layer belongs to whom. Every violation is printed, not only the
/// first, and none of them shows a value.
fn check_rules(
    files: meridian_rules::Files,
    set: &[String],
    data_files: &[PathBuf],
    state_dir: &Path,
) -> Result<(), Failure> {
    // Each --data file is a layer of its own, so a refusal can say which.
    let mut command_line = Vec::new();
    for file in data_files {
        let data = read_data(std::slice::from_ref(file), state_dir)?;
        command_line.push(meridian_rules::Layer::command_line(
            format!("--data {}", file.display()),
            data,
        ));
    }
    // The --set values are what they are on compile: data, merged last. They
    // are also what a layer path's %{KEY} reads.
    let mut values = Data::new();
    apply_sets(&mut values, set)?;
    let scope: meridian_resolve::Scope = set
        .iter()
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    if !values.is_empty() {
        command_line.push(meridian_rules::Layer::set("--set", values));
    }
    let checked = meridian_rules::check_files(&files, &scope, &command_line)
        .map_err(|refusal| Failure::Message(refusal.to_string()))?;
    let Some((path, rules)) = &checked.rules else {
        println!(
            "no rules file ({} beside {}): nothing was checked",
            meridian_rules::RULES_FILE,
            files.hierarchy.display()
        );
        return Ok(());
    };
    println!(
        "rules: {} ({})",
        path.display(),
        count(rules.rules.len(), "rule")
    );
    println!("layers, least specific first:");
    let width = checked
        .groups
        .iter()
        .map(|(p, _)| p.len())
        .max()
        .unwrap_or(0);
    for (layer, group) in &checked.groups {
        if rules.groups.is_empty() {
            println!("  {layer}");
        } else {
            println!("  {layer:width$}  {group}");
        }
    }
    println!("no violations");
    Ok(())
}

/// Applies the `--set key=value` options to the data, in the order given.
/// A dotted key sets a value inside a mapping, creating the mappings on the
/// way; the other keys of those mappings are kept.
pub(crate) fn apply_sets(data: &mut Data, sets: &[String]) -> Result<(), Failure> {
    use meridian_ir::Value;
    for set in sets {
        let refuse = |why: &str| Failure::Message(format!("--set {set}: {why}"));
        let Some((key, raw)) = set.split_once('=') else {
            return Err(refuse("expected KEY=VALUE, as in --set env=prod"));
        };
        let path: Vec<&str> = key.split('.').collect();
        if path.iter().any(|part| part.trim().is_empty()) {
            return Err(refuse(
                "the key is empty, or has an empty part between dots",
            ));
        }
        // The value is one YAML scalar, list or mapping. Text that YAML
        // cannot read as a value is taken as the text it is.
        let value = match meridian_ir::parse_value("--set", raw.as_bytes()) {
            Ok(value) => value,
            Err(_) => Value::String(raw.to_owned()),
        };
        let mut holder = &mut *data;
        for (i, part) in path[..path.len() - 1].iter().enumerate() {
            let entry = holder
                .entry((*part).to_owned())
                .or_insert_with(|| Value::Map(Data::new()));
            holder = match entry {
                Value::Map(inner) => inner,
                other => {
                    return Err(refuse(&format!(
                        "{} is already set to {}, so it cannot hold the key {:?}",
                        path[..=i].join("."),
                        other.describe(),
                        path[i + 1]
                    )))
                }
            };
        }
        holder.insert(path[path.len() - 1].to_owned(), value);
    }
    Ok(())
}

/// The directories plugins, and backend programs, are looked for in.
fn plugin_dirs(plugins: &PluginDirs) -> Vec<PathBuf> {
    if plugins.dirs.is_empty() {
        own_directory().into_iter().collect()
    } else {
        plugins.dirs.clone()
    }
}

/// Where Meridian's record is kept, as the command line chose it.
fn backend(state: &State) -> Result<Arc<dyn Backend>, Failure> {
    let mut settings = BTreeMap::new();
    for setting in &state.backend_config {
        let Some((key, value)) = setting.split_once('=').filter(|(key, _)| !key.is_empty()) else {
            return Err(Failure::Message(format!(
                "--backend-config takes key=value, and was given {setting:?}"
            )));
        };
        settings.insert(key.to_owned(), value.to_owned());
    }
    if state.backend == "local" {
        if let Some(key) = settings.keys().next() {
            return Err(Failure::Message(format!(
                "the local backend takes no settings, and was given {key:?}; it keeps the \
                 record in --state-dir"
            )));
        }
        return Ok(Arc::new(LocalBackend::new(&state.state_dir)));
    }
    ExecBackend::find(&state.backend, &plugin_dirs(&state.plugins), settings)
        .map(|found| Arc::new(found) as Arc<dyn Backend>)
        .map_err(Failure::Message)
}

pub(crate) fn registry(plugins: &PluginDirs) -> Result<Registry, Failure> {
    let dirs = plugin_dirs(plugins);
    let options = StartOptions {
        verification: plugins.verification()?,
        ..StartOptions::default()
    };
    let mut registry = Registry::new();
    for dir in dirs {
        let loaded = registry
            .discover(&dir.to_string_lossy(), &options)
            .map_err(|e| Failure::Message(e.to_string()))?;
        // Under `warn` a plugin that did not pass was started all the same;
        // this is the saying so.
        for info in loaded {
            match &info.signature {
                Signature::Unsigned => {
                    eprintln!("warning: plugin {} is not signed", info.path)
                }
                Signature::Invalid(why) => eprintln!(
                    "warning: plugin {}: its signature does not pass: {why}",
                    info.path
                ),
                Signature::NotChecked | Signature::Verified { .. } => {}
            }
        }
    }
    Ok(registry)
}

fn signing(command: SigningCommand) -> Result<(), Failure> {
    match command {
        SigningCommand::Keygen { secret, public } => {
            // Neither file is replaced: writing over a secret key loses the
            // only thing that can make signatures its public key accepts.
            for path in [&secret, &public] {
                if path.exists() {
                    return Err(Failure::Message(format!(
                        "{} already exists; a key is not written over",
                        path.display()
                    )));
                }
            }
            let key = SecretKey::generate().map_err(Failure::Message)?;
            write_secret(&secret, &format!("{}\n", key.to_text()))
                .and_then(|()| std::fs::write(&public, format!("{}\n", key.public().to_text())))
                .map_err(|e| Failure::Message(format!("writing the key pair: {e}")))?;
            println!("key {}", key.public().id());
            println!("  secret: {}", secret.display());
            println!("  public: {}", public.display());
            Ok(())
        }
        SigningCommand::Sign { key, binaries } => {
            let key = SecretKey::read(&key).map_err(Failure::Message)?;
            for binary in binaries {
                let text = key.sign_file(&binary).map_err(Failure::Message)?;
                let path = signature_path(&binary);
                std::fs::write(&path, text)
                    .map_err(|e| Failure::Message(format!("writing {}: {e}", path.display())))?;
                println!("signed {} with key {}", binary.display(), key.public().id());
            }
            Ok(())
        }
        SigningCommand::Verify { binaries } => {
            let trusted = trusted_keys()?;
            let mut failed = false;
            for binary in binaries {
                let found = meridian_plugin_host::signature::verify_file(&binary, &trusted);
                failed |= !matches!(found, Signature::Verified { .. });
                println!("{}: {found}", binary.display());
            }
            if failed {
                Err(Failure::Reported)
            } else {
                Ok(())
            }
        }
    }
}

/// Writes a secret key so that only its owner can read it, where the system
/// has such a thing as a file mode.
fn write_secret(path: &Path, text: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())
}

/// The directory the `meridian` binary really is in.
///
/// A package manager often puts a link to the binary on the `PATH` and keeps
/// the binary itself, with its plugins beside it, somewhere else. The link is
/// followed, so the plugins are found wherever the command was started from.
fn own_directory() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = meridian_sdk::tool::absolute_path(&exe);
    exe.parent().map(Path::to_path_buf)
}

fn import_configuration(plugins: &PluginDirs, request: &import::Request) -> Result<(), Failure> {
    let registry = registry(plugins)?;
    let outcome = import::run(&registry, request);
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    outcome
}

fn list_plugins(plugins: &PluginDirs) -> Result<(), Failure> {
    let registry = registry(plugins)?;
    let names = registry.names();
    if names.is_empty() {
        println!("no plugins found");
        return Ok(());
    }
    for name in names {
        let info = registry
            .target(name)
            .expect("a listed target exists")
            .info();
        let mut can = vec!["compile"];
        if info.operations.plan {
            can.push("plan");
        }
        if info.operations.apply {
            can.push("apply");
        }
        if info.operations.outputs {
            can.push("outputs");
        }
        if info.accepts_inputs {
            can.push("inputs");
        }
        if !info.import_formats.is_empty() {
            can.push("import");
        }
        println!("{name}");
        println!("  category:  {}", info.category);
        println!("  can:       {}", can.join(", "));
        println!("  resources: {}", info.resource_types.join(", "));
        if !info.target_versions.is_empty() {
            println!("  versions:  {}", info.target_versions.join(", "));
        }
        for format in &info.import_formats {
            println!(
                "  imports:   {} ({})",
                format.description,
                format.patterns.join(", ")
            );
        }
        println!(
            "  sdk:       {} (protocol {})",
            info.sdk_version, info.protocol_version
        );
        println!("  binary:    {}", info.path);
        println!("  signature: {}", info.signature);
    }
    registry
        .close()
        .map_err(|e| Failure::Message(e.to_string()))
}

/// Says where each declared secret comes from and whether that place would
/// answer. Nothing is fetched: a file is looked at and not read, a command is
/// found and not run. Fails when a secret that is not optional has no answer.
fn check_secrets(files: &[PathBuf], state_dir: &Path) -> Result<(), Failure> {
    use meridian_orchestrator::check_declared;
    use meridian_secret::Probe;
    let bundle = load(files, state_dir)?;
    let (mut declared, mut unanswered) = (0, 0);
    for doc in &bundle.documents {
        let rows = check_declared(doc);
        if rows.is_empty() {
            continue;
        }
        println!("{} ({})", doc.name(), doc.position().file);
        let width = rows.iter().map(|row| row.name.len()).max().unwrap_or(0);
        // Each row is a declaration and what a probe found. No value is
        // read by this command.
        for row in &rows {
            declared += 1;
            let state = match (&row.probe, row.optional) {
                (Probe::Present(_), _) => "ok",
                (Probe::Unknown(_), _) => "not checked",
                (Probe::Absent(_), true) => "absent, optional",
                (Probe::Absent(_), false) => "MISSING",
                (Probe::Refused(_), _) => "REFUSED",
            };
            if row.unanswered() {
                unanswered += 1;
            }
            println!(
                "  {:width$}  {:5}  {state}: {}",
                row.name,
                row.from,
                row.probe.words()
            );
        }
    }
    if declared == 0 {
        println!("no secret is declared");
    }
    if unanswered == 0 {
        Ok(())
    } else {
        Err(Failure::Message(format!(
            "{} would not be found by plan or apply",
            count(unanswered, "secret")
        )))
    }
}

/// What a command adds to the workflow it runs.
#[derive(Default)]
struct How<'a> {
    /// `compile --out`
    out: Option<&'a Path>,
    /// `apply --resume`
    resume: Option<&'a str>,
    /// `apply --on-failure rollback`
    rollback_on_failure: bool,
    /// `rollback --run`
    rollback_of: Option<&'a str>,
}

fn orchestrate(workflow: &Workflow, mode: Mode, how: &How<'_>) -> Result<(), Failure> {
    let (out, resume) = (how.out, how.resume);
    let bundle = load(&workflow.files, &workflow.state.state_dir)?;
    let mut selector = Selector::all();
    for text in &workflow.selector {
        selector = selector.and(Selector::parse(text).map_err(Failure::Message)?);
    }
    let mut data = read_data(&workflow.data, &workflow.state.state_dir)?;
    apply_sets(&mut data, &workflow.set)?;
    if selected(&bundle, &selector, &data) == 0 {
        // Doing nothing and reporting success would read as "all applied".
        return Err(Failure::Message(format!(
            "no document's labels match the selector {selector}"
        )));
    }
    // What was typed, for the record: the files and the selection. Backend
    // settings are left out, because one of them can be a credential.
    let mut command = vec![mode.as_str().to_owned()];
    command.extend(workflow.files.iter().map(|f| f.display().to_string()));
    for text in &workflow.selector {
        command.push(format!("-l {text}"));
    }
    if resume.is_some() {
        command.push("--resume".to_owned());
    }
    if how.rollback_on_failure {
        command.push("--on-failure rollback".to_owned());
    }
    for path in &workflow.policy {
        command.push(format!("--policy {}", path.display()));
    }
    let policy = if workflow.policy.is_empty() {
        None
    } else if !matches!(mode, Mode::Plan | Mode::Apply) {
        // Saying nothing would read as "checked".
        return Err(Failure::Message(format!(
            "--policy is checked by plan and apply, and {} asks no policy; remove it",
            mode.as_str()
        )));
    } else {
        Some(
            Policy::find(
                &workflow.policy_engine,
                &plugin_dirs(&workflow.state.plugins),
                workflow.policy.clone(),
            )
            .map_err(Failure::Message)?,
        )
    };
    let options = Options {
        data,
        // Compiling keeps no record, so it needs no backend to be there.
        backend: match mode {
            Mode::Compile => None,
            Mode::Plan | Mode::Apply | Mode::Drift | Mode::Rollback => {
                Some(backend(&workflow.state)?)
            }
        },
        lock_timeout: workflow.lock_timeout,
        resume: resume.map(|run| match run {
            "last" => Resume::Last,
            id => Resume::Run(id.to_owned()),
        }),
        command: command.join(" "),
        keep_runs: workflow.keep_runs,
        on_failure: if how.rollback_on_failure {
            OnFailure::Rollback
        } else {
            OnFailure::Stop
        },
        rollback_of: match how.rollback_of {
            None => Resume::Last,
            Some(id) => Resume::Run(id.to_owned()),
        },
        policy,
        strict_secrets: workflow.strict_secrets,
        ..Options::new(&workflow.state.state_dir)
    };
    let registry = registry(&workflow.state.plugins)?;
    let mut printer = Printer {
        mode,
        out,
        events: workflow.events.is_some(),
    };
    let report = run_selected(&bundle, &registry, mode, &options, &selector, &mut printer);
    // A plugin that will not exit is worth a warning, not a failed run.
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if let Some(refusal) = &report.refused {
        return Err(Failure::Message(refusal.to_string()));
    }
    if mode == Mode::Drift {
        let drifted = report
            .stages
            .iter()
            .filter(|s| matches!(s.outcome, Outcome::Drift(Drift::Drifted(_))))
            .count();
        let line = format!(
            "drift: {drifted} of {} drifted",
            count(report.stages.len(), "stage")
        );
        if workflow.events.is_some() {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    if !report.succeeded() {
        let failed: Vec<&StageReport> = report
            .stages
            .iter()
            .filter(|s| matches!(s.outcome, Outcome::Failed(_)))
            .collect();
        let denied = !failed.is_empty() && failed.iter().all(|s| s.reason == REASON_POLICY_DENIED);
        let applied = report
            .stages
            .iter()
            .any(|s| matches!(s.outcome, Outcome::Applied { .. }));
        if denied && mode == Mode::Apply && !applied {
            eprintln!(
                "run {} was stopped by its policy before anything was applied",
                report.run_id
            );
        } else if mode == Mode::Apply && report.rollbacks.is_empty() {
            eprintln!(
                "run {} failed. Once the cause is fixed, `meridian apply --resume` with the \
                 same documents continues from the stage that failed",
                report.run_id
            );
        }
        return Err(Failure::Reported);
    }
    if report.drifted() {
        return Err(Failure::Drifted);
    }
    Ok(())
}

/// Shows the current or last run, and whether the state is locked.
fn status(state: &State) -> Result<(), Failure> {
    let backend = backend(state)?;
    let backend = backend.as_ref();
    let held = lock::holder(backend).map_err(Failure::Message)?;
    match record::last_run(backend, None).map_err(Failure::Message)? {
        None => println!("no run has been recorded in {}", backend.describe()),
        Some(run) => print_run(&run, held.as_ref().is_some_and(|lock| lock.id == run.id)),
    }
    match held {
        Some(lock) => println!("locked: {lock}"),
        None => println!("not locked"),
    }
    Ok(())
}

fn print_run(run: &RunRecord, holds_lock: bool) {
    let status = match (run.status, holds_lock) {
        (record::RunStatus::Running, false) => {
            "interrupted: it never said how it ended, and it does not hold the lock"
        }
        (status, _) => status.as_str(),
    };
    println!("run {}: {}, {status}", run.id, run.mode);
    if !run.command.is_empty() {
        println!("  command:  meridian {}", run.command);
    }
    let by = match (run.user.as_str(), run.host.as_str()) {
        ("", "") => String::new(),
        (user, host) => format!(
            " by {}@{}",
            if user.is_empty() { "?" } else { user },
            if host.is_empty() { "?" } else { host }
        ),
    };
    println!("  started:  {}{by}", run.started);
    if !run.finished.is_empty() {
        println!("  finished: {}", run.finished);
    }
    if !run.resumes.is_empty() {
        println!("  resumes:  {}", run.resumes);
    }
    for step in &run.steps {
        let on = if step.endpoint.is_empty() {
            String::new()
        } else {
            format!(" on {}", step.endpoint)
        };
        let mut line = format!(
            "  {} ({}, target {}{on}): {}",
            step.id.name,
            step.id.kind.as_str(),
            step.target,
            step.status.as_str()
        );
        if !step.reason.is_empty() {
            line.push_str(&format!(" ({})", step.reason));
        }
        if step.changes > 0 {
            line.push_str(&format!(", {}", count(step.changes as usize, "change")));
        }
        println!("{line}");
    }
}

/// Lists every recorded run, oldest first.
fn runs(state: &State) -> Result<(), Failure> {
    let backend = backend(state)?;
    let backend = backend.as_ref();
    let ids = record::list_runs(backend).map_err(Failure::Message)?;
    if ids.is_empty() {
        println!("no run has been recorded in {}", backend.describe());
        return Ok(());
    }
    for id in ids {
        let Some(run) = record::load_run(backend, &id).map_err(Failure::Message)? else {
            continue;
        };
        let failed = run
            .steps
            .iter()
            .filter(|step| step.status == record::StepStatus::Failed)
            .count();
        let mut line = format!(
            "{}  {:<8} {:<10} {}",
            run.id,
            run.mode,
            run.status.as_str(),
            count(run.steps.len(), "step")
        );
        if failed > 0 {
            line.push_str(&format!(", {failed} failed"));
        }
        if !run.resumes.is_empty() {
            line.push_str(&format!(", resumes {}", run.resumes));
        }
        println!("{line}");
    }
    Ok(())
}

/// Prints a run's events as they were recorded.
fn events(state: &State, run: Option<&str>, types: &[String]) -> Result<(), Failure> {
    let backend = backend(state)?;
    let backend = backend.as_ref();
    let id = match run {
        Some(id) => {
            if record::load_run(backend, id)
                .map_err(Failure::Message)?
                .is_none()
            {
                return Err(Failure::Message(format!(
                    "there is no run {id} in {}",
                    backend.describe()
                )));
            }
            id.to_owned()
        }
        None => match record::last_run(backend, None).map_err(Failure::Message)? {
            Some(run) => run.id,
            None => {
                return Err(Failure::Message(format!(
                    "no run has been recorded in {}",
                    backend.describe()
                )))
            }
        },
    };
    let mut out = std::io::stdout().lock();
    for event in record::load_events(backend, &id).map_err(Failure::Message)? {
        if types.is_empty() || types.iter().any(|pattern| event.matches(pattern)) {
            let _ = writeln!(out, "{}", event.to_json());
        }
    }
    Ok(())
}

/// Removes a lock a run left behind.
fn unlock(state: &State, id: &str) -> Result<(), Failure> {
    let backend = backend(state)?;
    let removed = lock::force_unlock(backend.as_ref(), id).map_err(Failure::Message)?;
    println!("removed {removed}");
    Ok(())
}

/// Prints a run as it happens.
struct Printer<'a> {
    mode: Mode,
    out: Option<&'a Path>,
    /// Whether standard output carries the events, which moves the report
    /// for people to standard error.
    events: bool,
}

impl Observer for Printer<'_> {
    fn log(&mut self, id: &StageId, line: &LogLine) {
        // The tools' own chatter goes to stderr, so stdout stays a clean
        // record of what Meridian found and did.
        let mark = match line.stream {
            LogStream::Stdout => "|",
            LogStream::Stderr => "!",
        };
        eprintln!("  {} {mark} {}", id.name, line.text);
    }

    fn stage_finished(&mut self, report: &StageReport) {
        if self.events {
            let _ = self.print(&mut std::io::stderr().lock(), report);
        } else {
            let _ = self.print(&mut std::io::stdout().lock(), report);
        }
    }

    fn event(&mut self, event: &Event) {
        if self.events {
            let _ = writeln!(std::io::stdout().lock(), "{}", event.to_json());
        }
    }

    fn rollback_finished(&mut self, report: &RollbackReport) {
        if self.events {
            let _ = print_rollback(&mut std::io::stderr().lock(), report);
        } else {
            let _ = print_rollback(&mut std::io::stdout().lock(), report);
        }
    }
}

/// The first characters of an artifact hash, enough to tell two apart.
fn short(hash: &str) -> &str {
    let hex = hash.strip_prefix("sha256:").unwrap_or(hash);
    &hex[..hex.len().min(12)]
}

fn print_rollback(out: &mut dyn Write, report: &RollbackReport) -> std::io::Result<()> {
    let on = if report.host.is_empty() {
        String::new()
    } else {
        format!(" on {}", report.host)
    };
    writeln!(
        out,
        "rolling back {} ({}, target {}{on})",
        report.id.name,
        report.id.kind.as_str(),
        report.target
    )?;
    match &report.outcome {
        RollbackOutcome::RolledBack { to, changes } => {
            for change in changes {
                writeln!(out, "  {}", change_line(change))?;
            }
            writeln!(
                out,
                "  rolled back to what was applied before (artifact {}), {}",
                short(to),
                count(changes.len(), "change")
            )
        }
        RollbackOutcome::Unchanged => writeln!(
            out,
            "  nothing to put back: the run applied what was already applied"
        ),
        RollbackOutcome::Unavailable(why) => writeln!(out, "  NOT ROLLED BACK: {why}"),
        RollbackOutcome::Failed(why) => {
            let mut lines = why.lines();
            writeln!(
                out,
                "  ROLLBACK FAILED: {}",
                lines.next().unwrap_or_default()
            )?;
            for line in lines {
                writeln!(out, "    {line}")?;
            }
            Ok(())
        }
        RollbackOutcome::NotAttempted { because_of } => writeln!(
            out,
            "  not attempted, because rolling back {because_of} failed"
        ),
    }
}

impl Printer<'_> {
    fn print(&self, out: &mut dyn Write, report: &StageReport) -> std::io::Result<()> {
        let on = if report.host.is_empty() {
            String::new()
        } else {
            format!(" on {}", report.host)
        };
        writeln!(
            out,
            "{} ({}, target {}{on})",
            report.id.name,
            report.id.kind.as_str(),
            report.target
        )?;
        for warning in &report.warnings {
            writeln!(out, "  warning: {warning}")?;
        }
        if let Some(policy) = &report.policy {
            for warning in &policy.warn {
                writeln!(out, "  policy warning: {warning}")?;
            }
            if policy.allowed() {
                writeln!(out, "  policy: allowed")?;
            }
        }
        // Every decision a condition made, so that something missing from
        // the compiled output is never a surprise.
        if !report.removed.is_empty() {
            writeln!(out, "  when: {} removed", report.removed.len())?;
            for removed in &report.removed {
                writeln!(
                    out,
                    "    {}: {} is false",
                    removed.subject, removed.condition
                )?;
            }
        }

        match &report.outcome {
            Outcome::Compiled => {
                let files = report.artifact.as_ref().map(|a| a.files.len()).unwrap_or(0);
                match (self.mode, self.out) {
                    (Mode::Compile, Some(dir)) => {
                        let dir = dir.join(&report.id.name);
                        match write_artifact(&dir, report) {
                            Ok(()) => writeln!(
                                out,
                                "  compiled {} into {}",
                                count(files, "file"),
                                dir.display()
                            )?,
                            Err(e) => writeln!(out, "  error: writing {}: {e}", dir.display())?,
                        }
                    }
                    (Mode::Compile, None) => {
                        writeln!(out, "  compiled {}", count(files, "file"))?;
                        if let Some(artifact) = &report.artifact {
                            for path in artifact.files.keys() {
                                writeln!(out, "    {path}")?;
                            }
                        }
                    }
                    _ => writeln!(
                        out,
                        "  compiled {}; this target only compiles, so there is no preview",
                        count(files, "file")
                    )?,
                }
            }
            Outcome::Planned(plan) => {
                for change in &plan.changes {
                    writeln!(out, "  {}", change_line(change))?;
                }
                if plan.unknown {
                    writeln!(out, "  this tool cannot preview; applying may change things")?;
                } else if plan.changes.is_empty() {
                    writeln!(out, "  in sync, nothing to change")?;
                } else {
                    writeln!(out, "  {} to make", count(plan.changes.len(), "change"))?;
                }
            }
            Outcome::Applied { changes, outputs } => {
                for change in changes {
                    writeln!(out, "  {}", change_line(change))?;
                }
                if changes.is_empty() {
                    writeln!(out, "  applied, nothing needed changing")?;
                } else {
                    writeln!(out, "  applied {}", count(changes.len(), "change"))?;
                }
                for output in outputs {
                    writeln!(out, "  output {} = {}", output.name, shown(output))?;
                }
            }
            Outcome::Deferred { waiting_on, output } => writeln!(
                out,
                "  deferred: needs the output {output:?} of {waiting_on}, which has not been applied yet"
            )?,
            Outcome::Drift(drift) => match drift {
                Drift::NeverApplied => writeln!(out, "  never applied, so nothing to compare")?,
                Drift::InSync => writeln!(out, "  in sync with what was applied")?,
                Drift::Unknown => writeln!(
                    out,
                    "  unknown: this tool cannot preview, so it cannot say whether anything \
                     drifted"
                )?,
                Drift::Drifted(changes) => {
                    for change in changes {
                        writeln!(out, "  {}", change_line(change))?;
                    }
                    writeln!(
                        out,
                        "  DRIFTED: {} from what was applied",
                        count(changes.len(), "difference")
                    )?;
                }
                Drift::Pending {
                    changes,
                    last_apply_failed,
                } => {
                    for change in changes {
                        writeln!(out, "  {}", change_line(change))?;
                    }
                    let why = if *last_apply_failed {
                        "the last apply failed part way"
                    } else {
                        "the document has changed since the last apply"
                    };
                    writeln!(
                        out,
                        "  pending: {why}, so {} waiting to be applied and cannot be told \
                         from drift",
                        match changes.len() {
                            0 => "nothing is".to_owned(),
                            1 => "1 change is".to_owned(),
                            n => format!("{n} changes are"),
                        }
                    )?;
                }
            },
            Outcome::Skipped { applied_in } => writeln!(
                out,
                "  already applied by run {applied_in}, and unchanged since; left alone"
            )?,
            Outcome::LeftOut { condition } => {
                writeln!(out, "  skipped: its when ({condition}) is false")?
            }
            Outcome::Failed(why) => {
                // What the tool had already done is part of what happened.
                for change in &report.done_before_failing {
                    writeln!(out, "  {}", change_line(change))?;
                }
                // The first line beside the label, the rest indented under it.
                let mut lines = why.lines();
                writeln!(out, "  FAILED: {}", lines.next().unwrap_or_default())?;
                for line in lines {
                    writeln!(out, "    {line}")?;
                }
            }
            Outcome::NotRun { because_of } => {
                writeln!(out, "  not run, because {because_of} failed")?
            }
        }
        Ok(())
    }
}

fn change_line(change: &meridian_sdk::Change) -> String {
    let sign = match change.action {
        ChangeAction::Create => '+',
        ChangeAction::Update => '~',
        ChangeAction::Delete => '-',
        ChangeAction::Replace => '!',
    };
    format!("{sign} {change}")
}

/// An output's value as shown to a person: never a sensitive one.
fn shown(output: &Output) -> String {
    if output.sensitive {
        MASK.to_owned()
    } else {
        output.value.canonical()
    }
}

/// Writes a compiled artifact under `dir`, refusing any path that would land
/// outside it. An artifact's paths come from a plugin, and a plugin is not
/// trusted to choose where on the operator's disk a file goes.
fn write_artifact(dir: &Path, report: &StageReport) -> std::io::Result<()> {
    let Some(artifact) = &report.artifact else {
        return Ok(());
    };
    for (relative, content) in &artifact.files {
        let path = Path::new(relative);
        let safe = !relative.is_empty()
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !safe {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("the plugin produced the unsafe path {relative:?}"),
            ));
        }
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }
    Ok(())
}
