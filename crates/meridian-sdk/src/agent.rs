//! An SSH agent started for one run, holding a credential's private key.
//!
//! A tool that reaches a host over SSH (`ssh` itself, `salt-ssh`, `bolt`,
//! `chef-run`) takes a private key from a file or from an agent. A file is
//! the plan's rank 2; an agent is rank 1: the key goes from protected memory
//! into `ssh-add`'s standard input, the agent holds it for the length of the
//! stage, and the tool is told only where the agent's socket is, through
//! `SSH_AUTH_SOCK`. Nothing of the key is on a disk, on a command line or in
//! the environment, and a key with a passphrase is added the same way, the
//! passphrase answered from a file only the user can read.
//!
//! The agent is `ssh-agent -D -a <socket>`, a child of the plugin that is
//! killed when the [`SshAgent`] is dropped; its socket is in a directory only
//! the user can enter, removed with it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use meridian_secret::{Part, SecretFile, SecretValue};

use crate::tool::Tool;
use crate::{Log, RunError};

/// Names the `ssh-agent` program, when it is not on `PATH`.
pub const SSH_AGENT_VARIABLE: &str = "MERIDIAN_SSH_AGENT";
/// Names the `ssh-add` program, when it is not on `PATH`.
pub const SSH_ADD_VARIABLE: &str = "MERIDIAN_SSH_ADD";
/// The variable every SSH client reads the agent's socket from.
pub const AUTH_SOCKET_VARIABLE: &str = "SSH_AUTH_SOCK";

/// How long to wait for a started agent to listen on its socket.
const LISTEN_WAIT: Duration = Duration::from_secs(10);

/// The programs an agent is started and loaded with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPrograms {
    pub ssh_agent: String,
    pub ssh_add: String,
}

impl Default for AgentPrograms {
    fn default() -> Self {
        AgentPrograms {
            ssh_agent: "ssh-agent".to_owned(),
            ssh_add: "ssh-add".to_owned(),
        }
    }
}

impl AgentPrograms {
    /// The programs as the environment names them, or the defaults.
    pub fn from_env() -> Self {
        let named = |variable: &str, default: &str| {
            std::env::var(variable)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        AgentPrograms {
            ssh_agent: named(SSH_AGENT_VARIABLE, "ssh-agent"),
            ssh_add: named(SSH_ADD_VARIABLE, "ssh-add"),
        }
    }
}

/// A running agent that holds one key. Dropping it stops the agent and
/// removes its socket's directory.
#[derive(Debug)]
pub struct SshAgent {
    child: Child,
    /// The passphrase, when the key has one, and the directory the socket
    /// is in. Held so that both outlive every use of the agent.
    files: SecretFile,
    socket: PathBuf,
}

impl SshAgent {
    /// Starts an agent and adds `key` to it, answering `passphrase` when the
    /// key asks for one. The key goes to `ssh-add` on its standard input.
    pub fn start(
        programs: &AgentPrograms,
        key: &SecretValue,
        passphrase: Option<&SecretValue>,
        log: &dyn Log,
    ) -> Result<SshAgent, RunError> {
        // The directory for the socket, and the passphrase if there is one.
        // Without a passphrase the file is empty: the directory is what is
        // needed.
        let parts: Vec<Part<'_>> = match passphrase {
            Some(passphrase) => vec![Part::Secret(passphrase), Part::Text("\n")],
            None => Vec::new(),
        };
        let files = SecretFile::create("passphrase", &parts)
            .map_err(|e| RunError::new(format!("preparing the SSH agent's directory: {e}")))?;
        let socket = files.dir().join("agent.sock");
        let socket_text = socket.to_string_lossy().into_owned();

        let child = Command::new(&programs.ssh_agent)
            .args(["-D", "-a", &socket_text])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    RunError::new(format!(
                        "could not run {}: it is not installed, or not on the PATH of the \
                         machine running Meridian; a credential's private key reaches a host \
                         through an SSH agent",
                        programs.ssh_agent
                    ))
                } else {
                    RunError::new(format!("could not run {}: {e}", programs.ssh_agent))
                }
            })?;
        // From here the agent is stopped on every path out.
        let mut agent = SshAgent {
            child,
            files,
            socket,
        };
        agent.wait_for_socket(&programs.ssh_agent)?;
        log.out("started an SSH agent for the credential's key");

        // OpenSSH wants the key to end in a line break; one more is harmless.
        let key = SecretValue::from_parts(&[Part::Secret(key), Part::Text("\n")])
            .map_err(|e| RunError::new(format!("preparing the key for ssh-add: {e}")))?;
        let mut add = Tool::new(&programs.ssh_add)
            .arg("-")
            .env(AUTH_SOCKET_VARIABLE, &socket_text)
            .stdin_secret(key);
        if passphrase.is_some() {
            // ssh-add asks for a passphrase through the program SSH_ASKPASS
            // names, when it has no terminal; the program reads the file
            // beside the socket and prints it, and nothing else sees it.
            let script = agent
                .files
                .beside(
                    "askpass",
                    &format!(
                        "#!/bin/sh\nexec cat {}\n",
                        shell_word(&agent.files.path().to_string_lossy())
                    ),
                )
                .map_err(|e| RunError::new(format!("writing the askpass program: {e}")))?;
            make_executable(&script)
                .map_err(|e| RunError::new(format!("writing the askpass program: {e}")))?;
            add = add
                .env("SSH_ASKPASS", script.to_string_lossy().into_owned())
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env(
                    "DISPLAY",
                    std::env::var("DISPLAY").unwrap_or_else(|_| "meridian:0".to_owned()),
                );
        }
        add.run_ok(
            "ssh-add could not add the credential's key to the agent",
            log,
        )?;
        Ok(agent)
    }

    fn wait_for_socket(&mut self, program: &str) -> Result<(), RunError> {
        let started = Instant::now();
        loop {
            if self.socket.exists() {
                return Ok(());
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                let mut said = String::new();
                if let Some(mut stderr) = self.child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut stderr, &mut said);
                }
                return Err(RunError {
                    msg: format!("{program} exited ({status}) before it listened on its socket"),
                    output: said.trim().to_owned(),
                    ..RunError::default()
                });
            }
            if started.elapsed() > LISTEN_WAIT {
                return Err(RunError::new(format!(
                    "{program} did not listen on {} within {} seconds",
                    self.socket.display(),
                    LISTEN_WAIT.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The agent's socket, which a tool is told about through
    /// [`AUTH_SOCKET_VARIABLE`].
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `tool`, told where the agent is.
    pub fn tell(&self, tool: Tool) -> Tool {
        tool.env(
            AUTH_SOCKET_VARIABLE,
            self.socket.to_string_lossy().into_owned(),
        )
    }
}

impl Drop for SshAgent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The socket goes with the directory, when the secret file is dropped.
    }
}

/// `text` as one word for `sh`.
fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Starts an agent for the credential's key, when it has one. A credential
/// with `agent: true`, or with no key, uses the tool's own environment and
/// starts nothing.
pub fn agent_for(
    programs: &AgentPrograms,
    credential: Option<&crate::HeldCredential>,
    log: &dyn Log,
) -> Result<Option<SshAgent>, RunError> {
    let Some(credential) = credential else {
        return Ok(None);
    };
    let Some(key) = credential.secret("privateKey") else {
        return Ok(None);
    };
    SshAgent::start(programs, key, credential.secret("passphrase"), log).map(Some)
}
