//! The half that applies a compiled manifest with OpenVox.
//!
//! Applying is the Puppet plugin's runner: OpenVox keeps the `puppet` command,
//! its arguments, its exit statuses and its report, and OpenBolt keeps
//! `bolt`'s. What this module adds is everything that differs between the two
//! products on a real machine: where the tool is, how it is configured, and
//! whether the `puppet` that was found is the one the document asked for.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, Applied, Artifact, Log, NoLog, Operations, Plan, RunContext, RunError, Runner,
    Value,
};
use meridian_target_puppet::{PuppetRunner, Transport};

/// Names OpenVox's `puppet` program, when it is not where it is looked for.
pub const OPENVOX_VARIABLE: &str = "MERIDIAN_OPENVOX";
/// Names OpenBolt's `bolt` program, when it is not where it is looked for.
pub const OPENBOLT_VARIABLE: &str = "MERIDIAN_OPENBOLT";
/// `local` or `bolt`, to override the choice made from the host.
pub const TRANSPORT_VARIABLE: &str = "MERIDIAN_OPENVOX_TRANSPORT";
/// Extra arguments for every run, split on whitespace.
pub const ARGS_VARIABLE: &str = "MERIDIAN_OPENVOX_ARGS";
/// `warn`, `strict` or `off`: what to do when the tool is not OpenVox.
pub const VERIFY_VARIABLE: &str = "MERIDIAN_OPENVOX_VERIFY";

/// Where the OpenVox and OpenBolt packages install their commands. The
/// packages do not put these on the `PATH` of a non-login shell, which is the
/// kind of shell a plugin is usually started from.
pub const PACKAGE_BIN: &[&str] = &["/opt/puppetlabs/bin", "/opt/puppetlabs/bolt/bin"];

/// The value of the `implementation` fact on OpenVox. Puppet has no such
/// fact.
const IMPLEMENTATION: &str = "openvox";

/// What to do when the `puppet` command turns out not to be OpenVox.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Verify {
    /// Run it anyway, without asking.
    Off,
    /// Say so on the log and run it anyway. Both products apply the same
    /// manifest the same way, so this is the default.
    #[default]
    Warn,
    /// Refuse to run it.
    Strict,
}

impl Verify {
    /// Reads a setting. Anything unrecognised is the default, as an unset
    /// variable is.
    pub fn parse(text: &str) -> Verify {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" | "false" | "no" | "0" => Verify::Off,
            "strict" | "require" | "error" => Verify::Strict,
            _ => Verify::Warn,
        }
    }
}

/// Applies a compiled manifest with OpenVox.
///
/// A plan is the tool's own `--noop` run. OpenVox reports no outputs.
pub struct OpenVoxRunner {
    /// The programs, transport and extra arguments. The Puppet plugin's
    /// runner does the applying.
    pub tool: PuppetRunner,
    pub verify: Verify,
    /// Whether the program was found to be OpenVox, once it has been asked.
    checked: Mutex<Option<Implementation>>,
}

/// What the tool said it is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Implementation {
    OpenVox,
    /// Anything else, with what the fact read; empty when there was none.
    Other(String),
}

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The program to run for `name`: the bare name when it is on `path`, so the
/// operator's own choice of installation wins, and otherwise its full path in
/// the first of `fallbacks` that holds it. A program found nowhere stays the
/// bare name, and running it reports that it is not installed.
pub fn find_program(name: &str, path: Option<&OsStr>, fallbacks: &[&Path]) -> String {
    let on_path = path
        .map(|p| std::env::split_paths(p).collect::<Vec<PathBuf>>())
        .unwrap_or_default();
    if on_path
        .iter()
        .any(|dir| program_at(&dir.join(name)).is_some())
    {
        return name.to_owned();
    }
    fallbacks
        .iter()
        .find_map(|dir| program_at(&dir.join(name)))
        .and_then(|found| found.to_str().map(str::to_owned))
        .unwrap_or_else(|| name.to_owned())
}

/// The file that running `path` would start, if there is one. On Windows a
/// program is known by its extension, so that is what is looked for, and the
/// answer carries it: `puppet` is found as `puppet.bat`.
fn program_at(path: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .then(|| path.to_path_buf())
    }
    #[cfg(not(unix))]
    {
        ["exe", "cmd", "bat"]
            .iter()
            .map(|ext| path.with_extension(ext))
            .find(|candidate| candidate.is_file())
    }
}

impl OpenVoxRunner {
    /// A runner over the given programs, with the default transport and
    /// verification.
    pub fn new(puppet: impl Into<String>, bolt: impl Into<String>) -> Self {
        OpenVoxRunner {
            tool: PuppetRunner {
                puppet: puppet.into(),
                bolt: bolt.into(),
                transport: None,
                extra_args: Vec::new(),
            },
            verify: Verify::default(),
            checked: Mutex::new(None),
        }
    }

    /// The same runner with another answer to a tool that is not OpenVox.
    pub fn with_verify(mut self, verify: Verify) -> Self {
        self.verify = verify;
        self
    }

    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let path = std::env::var_os("PATH");
        let fallbacks: Vec<&Path> = PACKAGE_BIN.iter().map(Path::new).collect();
        let program = |variable_name: &str, name: &str| {
            variable(variable_name)
                .unwrap_or_else(|| find_program(name, path.as_deref(), &fallbacks))
        };
        let mut runner = OpenVoxRunner::new(
            program(OPENVOX_VARIABLE, "puppet"),
            program(OPENBOLT_VARIABLE, "bolt"),
        );
        runner.tool.transport = match variable(TRANSPORT_VARIABLE)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "local" => Some(Transport::Local),
            "bolt" => Some(Transport::Bolt),
            _ => None,
        };
        runner.tool.extra_args = variable(ARGS_VARIABLE)
            .map(|v| v.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        runner.verify = variable(VERIFY_VARIABLE)
            .map(|v| Verify::parse(&v))
            .unwrap_or_default();
        runner
    }

    /// Asks the tool what it is, once, and acts on the answer as
    /// [`Verify`] says.
    ///
    /// Only a run on this machine is checked. Over OpenBolt the catalog is
    /// applied by whatever agent the node has installed, and asking the node
    /// would cost a connection before every run.
    fn check(&self, context: &RunContext, log: &dyn Log) -> Result<(), RunError> {
        if self.verify == Verify::Off || self.tool.transport_for(&context.host) != Transport::Local
        {
            return Ok(());
        }
        let found = {
            let mut checked = self.checked.lock().expect("no panic holds the answer");
            match &*checked {
                Some(found) => found.clone(),
                None => {
                    let found = self.ask()?;
                    *checked = Some(found.clone());
                    found
                }
            }
        };
        let Implementation::Other(fact) = found else {
            return Ok(());
        };
        let what = if fact.is_empty() {
            format!(
                "{} is not OpenVox: it has no implementation fact, so it is Puppet, or an OpenVox release from before the fact existed",
                self.tool.puppet
            )
        } else {
            format!(
                "{} is not OpenVox: its implementation fact reads \"{fact}\"",
                self.tool.puppet
            )
        };
        if self.verify == Verify::Strict {
            return Err(RunError::new(format!(
                "{what}. Point {OPENVOX_VARIABLE} at OpenVox's puppet command, or set {VERIFY_VARIABLE} to warn or off to run this one"
            )));
        }
        log.err(&format!(
            "Warning: {what}. It is run anyway; set {VERIFY_VARIABLE}=strict to refuse it instead"
        ));
        Ok(())
    }

    /// Reads the `implementation` fact. A tool that cannot be started is an
    /// error, the same one the run itself would have ended with; a tool that
    /// starts and cannot answer is simply not OpenVox.
    fn ask(&self) -> Result<Implementation, RunError> {
        let finished = Tool::new(&self.tool.puppet)
            .args(["facts", "show", "implementation", "--render-as", "json"])
            .capture_stdout()
            .run(&NoLog)?;
        let fact = decode_value(finished.stdout.trim().as_bytes())
            .ok()
            .and_then(|v| {
                v.as_map()?
                    .get("implementation")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        Ok(if finished.success() && fact == IMPLEMENTATION {
            Implementation::OpenVox
        } else {
            Implementation::Other(fact)
        })
    }
}

impl Runner for OpenVoxRunner {
    fn operations(&self) -> Operations {
        self.tool.operations()
    }

    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        self.check(context, log)?;
        self.tool.plan(context, artifact, log)
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        self.check(context, log)?;
        self.tool.apply(context, artifact, log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verify_setting_is_read_loosely_and_defaults_to_a_warning() {
        assert_eq!(Verify::parse("strict"), Verify::Strict);
        assert_eq!(Verify::parse(" Strict "), Verify::Strict);
        assert_eq!(Verify::parse("require"), Verify::Strict);
        assert_eq!(Verify::parse("off"), Verify::Off);
        assert_eq!(Verify::parse("0"), Verify::Off);
        assert_eq!(Verify::parse("warn"), Verify::Warn);
        assert_eq!(Verify::parse("something else"), Verify::Warn);
        assert_eq!(Verify::default(), Verify::Warn);
    }

    #[test]
    fn a_new_runner_chooses_its_transport_by_the_host() {
        let runner = OpenVoxRunner::new("puppet", "bolt");
        assert_eq!(runner.tool.transport_for("localhost"), Transport::Local);
        assert_eq!(runner.tool.transport_for(""), Transport::Local);
        assert_eq!(runner.tool.transport_for("web01"), Transport::Bolt);
        assert_eq!(
            runner.operations(),
            Operations {
                plan: true,
                apply: true,
                outputs: false
            }
        );
    }

    #[test]
    fn a_program_found_nowhere_keeps_its_bare_name() {
        let missing = Path::new("/nonexistent/meridian/openvox");
        assert_eq!(find_program("puppet", None, &[missing]), "puppet");
        assert_eq!(
            find_program("puppet", Some(OsStr::new("/nonexistent/a")), &[]),
            "puppet"
        );
    }
}
