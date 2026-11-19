//! The command line: every subcommand and option, as `clap` parses them,
//! and the small parsers for values that are more than a word.

use super::*;

/// Whose signatures count, for plugins and for modules alike, and whether
/// modules must carry one. These can be given to any command.
#[derive(Args)]
pub(crate) struct Trust {
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
    pub(crate) module_signatures: SignaturePolicy,
    #[command(flatten)]
    pub(crate) keys: TrustedKeys,
}

#[derive(Subcommand)]
pub(crate) enum Command {
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
        /// Exit 2 when an apply would do something, 0 when nothing would
        /// change and 1 on an error, so a pipeline can ask "is there
        /// anything to apply?" without reading the plan
        #[arg(long)]
        detailed_exitcode: bool,
        /// Save the plan to this file, for `apply --plan FILE` to carry out
        /// exactly: each stage's artifact hash, its changes, and the tool's
        /// own saved plan where the tool has one. Written only when every
        /// stage planned
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Make the documents' desired state real
    Apply {
        #[command(flatten)]
        workflow: Workflow,
        /// Continue an apply that failed or was interrupted: stages it
        /// already applied, from documents that have not changed since, are
        /// left alone. `--resume` continues the last apply; `--resume=RUN`
        /// a run by its id (with `=`, so that a file is never taken for a
        /// run)
        #[arg(
            long,
            value_name = "RUN",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "last"
        )]
        resume: Option<String>,
        /// What to do when a stage fails: stop, leaving what was applied in
        /// place, or rollback, putting every stage this run changed back to
        /// what was applied before
        #[arg(long, value_name = "ACTION", default_value = "stop")]
        on_failure: FailureAction,
        /// Carry out the plan `plan --out` saved here, and nothing else: the
        /// apply is refused when a document no longer compiles to what was
        /// planned, or a stage was applied since. The documents, data,
        /// values and selector not given on the command line are taken
        /// from the plan; those given must match it
        #[arg(long, value_name = "FILE", conflicts_with = "resume")]
        plan: Option<PathBuf>,
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
    /// Print the tool output a run kept on this machine: by default the
    /// failed steps of the last run, or every step when none failed
    Logs {
        /// The run; the last one when not given
        run: Option<String>,
        /// Only the step of this document
        #[arg(long, value_name = "NAME")]
        step: Option<String>,
        /// Only the step on this machine, as `meridian status` shows it
        #[arg(long, value_name = "ENDPOINT")]
        endpoint: Option<String>,
        /// Only the lines of one stream: stdout, stderr, or meridian for
        /// what Meridian added (a failure, or how much was cut)
        #[arg(long, value_name = "STREAM")]
        stream: Option<StreamFilter>,
        /// Only the lines whose text matches this regular expression
        #[arg(long, value_name = "RE")]
        grep: Option<String>,
        /// Only the last N lines of each step, after the other filters
        #[arg(long, value_name = "N")]
        tail: Option<usize>,
        /// Every step, also when some failed
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        state: State,
    },
    /// Write one archive of a run for a bug report: its record, its events,
    /// the tool output this machine kept, and the versions of meridian and
    /// of the plugins the run used. It sends nothing anywhere
    BugReport {
        /// The run; the last one when not given
        run: Option<String>,
        /// The file to write. Defaults to meridian-bug-report-<RUN>.tar.gz
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
        #[command(flatten)]
        state: State,
    },
    /// Write a report of a recorded run, or of a file of events: JSON,
    /// JUnit XML, Markdown, HTML or SARIF
    Report {
        /// The run; the last one when not given
        run: Option<String>,
        /// The report's format: json, junit, markdown, html or sarif
        #[arg(short, long, value_name = "FORMAT", default_value = "markdown", value_parser = Format::parse)]
        format: Format,
        /// Write the report to this file and not to standard output
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Read the events from this file, one JSON object per line as
        /// `--events json` writes them, and not from the state; `-` reads
        /// standard input
        #[arg(long, value_name = "FILE", conflicts_with = "run")]
        from: Option<PathBuf>,
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
        /// Also list each plugin's settings: the name to use under
        /// spec.settings in meridian.yaml, the environment variable, the
        /// default and what it does
        #[arg(long)]
        settings: bool,
        /// Also list each plugin's own refusal rules, with what each refuses
        /// and what to write instead. The general rules every target shares
        /// are on the refusal rules reference page
        #[arg(long)]
        rules: bool,
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
    /// Move a document to another target: rewrite spec.target and, when the
    /// new tool imports, write `adopt:` for each resource the old tool
    /// manages. Writes a new file and runs no tool but the old one's read
    Retarget {
        /// The file to move; YAML, left as it is
        file: PathBuf,
        /// The target to move to
        #[arg(long, value_name = "TARGET")]
        to: String,
        /// Where to write the moved file. Defaults to FILE with the target
        /// before its extension, as in site.opentofu.yaml
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Write no `adopt:` lines, and so read nothing from the old tool
        #[arg(long)]
        no_adopt: bool,
        /// Where the old tool's working files are kept, and fetched modules
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
        #[command(flatten)]
        plugins: PluginDirs,
    },
    /// Show every version this build carries: core, language, SDK and
    /// plugin protocol
    Version,
    /// Work with the secrets documents declare, without showing any
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    /// Run Test documents: compile or plan the documents each names, with
    /// the data each case gives, and check what came out
    Test {
        /// The test files (kind: Test). With none, every *.test.yaml under
        /// tests/ in the current directory
        files: Vec<PathBuf>,
        /// Run only the cases whose name holds this text
        #[arg(long, value_name = "TEXT")]
        case: Option<String>,
        /// Also write the results as JUnit XML to this file, for a pipeline
        /// engine to show as tests
        #[arg(long, value_name = "FILE")]
        junit: Option<PathBuf>,
        #[command(flatten)]
        plugins: PluginDirs,
        /// Where each case's scratch state is kept (under tests/ in it), and
        /// where fetched modules are, for a document written @module:path
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
    },
    /// Rewrite documents in the one standard layout, changing no value
    Fmt(tools::FmtArgs),
    /// Report errors, and things that compile but are probably mistakes
    Lint(tools::LintArgs),
    /// Print a JSON Schema of the document language, for YAML editors; or
    /// sync, show and clean the cache of each tool's own resource schemas
    Schema(tools::SchemaArgs),
    /// Run the language server, for editors, on standard input and output
    Lsp(tools::LspArgs),
    /// Run the MCP server, for AI models, on standard input and output
    Mcp(tools::McpArgs),
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

impl Command {
    /// The subcommand as it is typed, such as `plan` or `module verify`,
    /// which is what a JSON result names itself with.
    pub(crate) fn name(&self) -> String {
        match self {
            Command::Validate { .. } => "validate".to_owned(),
            Command::Compile { .. } => "compile".to_owned(),
            Command::Plan { .. } => "plan".to_owned(),
            Command::Apply { .. } => "apply".to_owned(),
            Command::Drift { .. } => "drift".to_owned(),
            Command::Rollback { .. } => "rollback".to_owned(),
            Command::Status { .. } => "status".to_owned(),
            Command::Runs { .. } => "runs".to_owned(),
            Command::Events { .. } => "events".to_owned(),
            Command::Logs { .. } => "logs".to_owned(),
            Command::BugReport { .. } => "bug-report".to_owned(),
            Command::Report { .. } => "report".to_owned(),
            Command::Unlock { .. } => "unlock".to_owned(),
            Command::Import { .. } => "import".to_owned(),
            Command::Plugins { .. } => "plugins".to_owned(),
            Command::Rules {
                command: RulesCommand::Check { .. },
            } => "rules check".to_owned(),
            Command::Migrate { .. } => "migrate".to_owned(),
            Command::Retarget { .. } => "retarget".to_owned(),
            Command::Version => "version".to_owned(),
            Command::Secret {
                command: SecretCommand::Check { .. },
            } => "secret check".to_owned(),
            Command::Secret {
                command: SecretCommand::Set { .. },
            } => "secret set".to_owned(),
            Command::Test { .. } => "test".to_owned(),
            Command::Fmt(_) => "fmt".to_owned(),
            Command::Lint(_) => "lint".to_owned(),
            Command::Schema(_) => "schema".to_owned(),
            Command::Lsp(_) => "lsp".to_owned(),
            Command::Mcp(_) => "mcp".to_owned(),
            Command::Signing { command } => format!(
                "signing {}",
                match command {
                    SigningCommand::Keygen { .. } => "keygen",
                    SigningCommand::Sign { .. } => "sign",
                    SigningCommand::Verify { .. } => "verify",
                }
            ),
            Command::Module { command, .. } => format!(
                "module {}",
                match command {
                    ModuleCommand::Install { .. } => "install",
                    ModuleCommand::Update { .. } => "update",
                    ModuleCommand::Verify => "verify",
                    ModuleCommand::List => "list",
                    ModuleCommand::Sign { .. } => "sign",
                    ModuleCommand::Pack { .. } => "pack",
                    ModuleCommand::Push { .. } => "push",
                }
            ),
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum ModuleCommand {
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
pub(crate) enum RulesCommand {
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
pub(crate) enum SigningCommand {
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
pub(crate) const RELEASE_PUBLIC_KEY: Option<&str> = option_env!("MERIDIAN_RELEASE_PUBLIC_KEY");

#[derive(Args, Default)]
pub(crate) struct TrustedKeys {
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
    pub(crate) trusted_keys: Vec<PathBuf>,
}

impl TrustedKeys {
    /// Every key to trust: the release's own, then the ones named.
    pub(crate) fn read(&self) -> Result<Vec<PublicKey>, Failure> {
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
                    keys.push(PublicKey::read(&file).map_err(|e| Failure::Message(e.into()))?);
                }
            } else {
                keys.push(PublicKey::read(path).map_err(|e| Failure::Message(e.into()))?);
            }
        }
        Ok(keys)
    }
}

/// What separates the directories in `MERIDIAN_PLUGIN_DIR`: the system's own
/// separator for a list of paths, since a Windows path holds a colon.
pub(crate) const PATH_LIST_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

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
    pub(crate) signatures: Option<SignaturePolicy>,
}

impl PluginDirs {
    pub(crate) fn verification(&self) -> Result<Verification, Failure> {
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
pub(crate) struct State {
    #[command(flatten)]
    pub(crate) plugins: PluginDirs,
    /// Where each tool's working files are kept, and with the local backend
    /// Meridian's own record: the run-state, the runs and the lock
    #[arg(
        long,
        value_name = "DIR",
        env = "MERIDIAN_STATE_DIR",
        default_value = ".meridian"
    )]
    pub(crate) state_dir: PathBuf,
    /// Where Meridian's own record is kept: `local`, the state directory,
    /// or the name of a backend program, meridian-backend-<NAME>, in a
    /// plugin directory
    #[arg(
        long,
        value_name = "NAME",
        env = "MERIDIAN_BACKEND",
        default_value = "local"
    )]
    pub(crate) backend: String,
    /// A setting for the backend, as key=value; may be given more than
    /// once. The backend program reads it as MERIDIAN_BACKEND_<KEY>
    #[arg(long, value_name = "KEY=VALUE")]
    pub(crate) backend_config: Vec<String>,
}

/// What an apply does when a stage fails.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum FailureAction {
    /// Stop, and leave what was applied in place
    Stop,
    /// Stop, then put back every stage this run changed
    Rollback,
}

/// How the events of a run are written to standard output.
#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum EventFormat {
    /// One JSON object per line
    Json,
}

#[derive(Args)]
pub(crate) struct Workflow {
    /// The documents that make up the workflow. May be left out only with
    /// `apply --plan`, which takes them from the plan
    pub(crate) files: Vec<PathBuf>,
    #[command(flatten)]
    pub(crate) state: State,
    /// How long to wait for another run to release the state lock, as in
    /// 30s, 5m or 1h. By default a locked state is refused at once
    #[arg(long, value_name = "DURATION", default_value = "0s", value_parser = parse_duration)]
    pub(crate) lock_timeout: Duration,
    /// Stop a stage whose tool has run this long, as in 30m or 2h, and fail
    /// the run as timed out. By default a tool may take as long as it takes
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub(crate) stage_timeout: Option<Duration>,
    /// Write every event of the run to standard output as it happens, one
    /// JSON object per line. The report for people moves to standard error
    #[arg(long, value_name = "FORMAT")]
    pub(crate) events: Option<EventFormat>,
    /// Write a report of the run when it ends, as FORMAT=FILE; the formats
    /// are json, junit, markdown, html and sarif; may be given more than once
    #[arg(long, value_name = "FORMAT=FILE")]
    pub(crate) report: Vec<String>,
    /// Send every event of the run, as it happens, to this sink: `command`,
    /// or the name of a sink program, meridian-notify-<NAME>, in a plugin
    /// directory; may be given more than once
    #[arg(
        long,
        value_name = "NAME",
        env = "MERIDIAN_SINKS",
        value_delimiter = ','
    )]
    pub(crate) sink: Vec<String>,
    /// A setting for a sink, as SINK.KEY=VALUE; may be given more than
    /// once. The sink program reads it as MERIDIAN_NOTIFY_<KEY>. Two keys
    /// are Meridian's own: `on`, the event types to send, as in
    /// run.*,step.failed, and `required`, true to fail the run when the
    /// sink fails
    #[arg(long, value_name = "SINK.KEY=VALUE")]
    pub(crate) sink_config: Vec<String>,
    /// How long a sink has to finish sending after the run's last event
    #[arg(long, value_name = "DURATION", default_value = "30s", value_parser = parse_duration)]
    pub(crate) sink_timeout: Duration,
    /// How many runs' records to keep; older ones are removed when a run ends
    #[arg(long, value_name = "N", default_value_t = meridian_orchestrator::DEFAULT_KEEP_RUNS)]
    pub(crate) keep_runs: usize,
    /// The most of one step's tool output kept in the state directory, as
    /// in 10MiB, 512KiB or a number of bytes. Past it the start and the end
    /// are kept, and a line says how much was cut between them
    #[arg(long, value_name = "SIZE", default_value = "10MiB", value_parser = parse_size)]
    pub(crate) log_limit: u64,
    /// Keep no tool output in the state directory; it is still printed
    #[arg(long, env = "MERIDIAN_NO_LOGS")]
    pub(crate) no_logs: bool,
    /// Run only the documents whose metadata.labels match, as in
    /// `tier=web,env!=prod`; may be given more than once, and every
    /// requirement must hold. Forms: key=value, key!=value, key, !key
    #[arg(short = 'l', long, value_name = "SELECTOR")]
    pub(crate) selector: Vec<String>,
    /// A YAML or JSON file of values that `when` conditions read as
    /// data.<key>; may be given more than once, and a later file's keys
    /// replace an earlier file's
    #[arg(long, value_name = "FILE")]
    pub(crate) data: Vec<PathBuf>,
    /// A policy file or directory that plan and apply check every stage
    /// against before anything is applied; may be given more than once. A
    /// change the policy denies, or a policy that cannot be asked, fails
    /// the run
    #[arg(long, value_name = "PATH")]
    pub(crate) policy: Vec<PathBuf>,
    /// The engine that reads the policies: the program
    /// meridian-policy-<NAME> in a plugin directory
    #[arg(long, value_name = "NAME", env = "MERIDIAN_POLICY_ENGINE", default_value = meridian_orchestrator::policy::DEFAULT_ENGINE)]
    pub(crate) policy_engine: String,
    /// Put every stage's compiled files through this analyzer, the program
    /// meridian-analyze-<NAME> in a plugin directory, as in `--analyze
    /// checkov,trivy`; may be given more than once. A finding at or above
    /// --fail-on fails the stage, and an analyzer that cannot be run fails
    /// it too
    #[arg(
        long,
        value_name = "NAME",
        env = "MERIDIAN_ANALYZERS",
        value_delimiter = ','
    )]
    pub(crate) analyze: Vec<String>,
    /// A setting for an analyzer, as NAME.KEY=VALUE; may be given more
    /// than once. The analyzer program reads it as MERIDIAN_ANALYZE_<KEY>
    #[arg(long, value_name = "NAME.KEY=VALUE")]
    pub(crate) analyze_config: Vec<String>,
    /// The severity at which an analyzer's finding fails its stage:
    /// critical, high, medium, low, info, or none to report findings and
    /// fail on nothing
    #[arg(long, value_name = "SEVERITY", env = "MERIDIAN_FAIL_ON", default_value = "high", value_parser = parse_fail_on)]
    pub(crate) fail_on: FailOn,
    /// A YAML or JSON file of findings this project accepts, under
    /// `accepted`; an accepted finding is reported and fails nothing
    #[arg(long, value_name = "FILE")]
    pub(crate) analyze_baseline: Option<PathBuf>,
    /// One value for `when` conditions, as in `--set env=prod` or
    /// `--set region.name=eu-west-1`; may be given more than once. It is
    /// applied after every --data file, so it wins. The value is typed as
    /// YAML would type it: true, 3 and null are not text, "3" is
    #[arg(long, value_name = "KEY=VALUE")]
    pub(crate) set: Vec<String>,
    /// Refuse to run when a secret cannot be locked in memory, instead of
    /// warning once and going on
    #[arg(long, env = "MERIDIAN_STRICT_SECRETS")]
    pub(crate) strict_secrets: bool,
}

#[derive(Subcommand)]
pub(crate) enum SecretCommand {
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
    /// Store a value in the first place a declared secret names that
    /// meridian can write (the kernel keyring, the Secret Service). The
    /// value is read from standard input, or typed at a prompt that does
    /// not show it; never from an argument
    Set {
        /// The secret's name, as the documents declare it
        name: String,
        /// The documents that declare it
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

fn parse_fail_on(text: &str) -> Result<FailOn, String> {
    text.parse()
}

/// A length of time as a person writes one: `90`, `30s`, `5m`, `1h`,
/// `250ms`. A bare number is seconds.
pub(crate) fn parse_duration(text: &str) -> Result<Duration, String> {
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

/// A size as a person writes one: `10MiB`, `512KiB`, `1GiB`, or a number
/// of bytes. K, M and G mean the same as KiB, MiB and GiB.
pub(crate) fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let (number, unit) = text.split_at(digits);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("{text:?} is not a size; write one as 10MiB or 512KiB"))?;
    let scale: u64 = match unit.trim() {
        "" | "B" => 1,
        "K" | "KiB" => 1 << 10,
        "M" | "MiB" => 1 << 20,
        "G" | "GiB" => 1 << 30,
        other => {
            return Err(format!(
                "{other:?} is not a unit of size; use KiB, MiB or GiB"
            ))
        }
    };
    Ok(number.saturating_mul(scale))
}

/// Reads one `--report FORMAT=FILE`.
pub(crate) fn parse_report(text: &str) -> Result<(Format, PathBuf), String> {
    match text.split_once('=') {
        Some((format, path)) if !path.is_empty() => Ok((Format::parse(format)?, path.into())),
        _ => Err(format!(
            "--report {text:?} is not FORMAT=FILE, as in junit=meridian.xml"
        )),
    }
}
