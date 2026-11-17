use std::path::PathBuf;

use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;

use crate::ServeError;

/// A plugin's listening endpoint: what to announce, and the connections that
/// arrive on it.
pub(crate) struct Listening {
    /// What the handshake line gives as the address to dial.
    pub address: String,
    pub incoming: UnixListenerStream,
    /// Removes the socket's directory when the plugin stops serving.
    pub _guard: Socket,
}

impl Listening {
    /// The handshake's name for this kind of endpoint.
    pub const NETWORK: &'static str = "unix";

    pub fn start() -> Result<Listening, ServeError> {
        let socket = Socket::create()?;
        let listener = UnixListener::bind(&socket.path)
            .map_err(|e| ServeError::new("listening on the plugin socket", e))?;
        Ok(Listening {
            address: socket.path.display().to_string(),
            incoming: UnixListenerStream::new(listener),
            _guard: socket,
        })
    }
}

/// The plugin's socket, in a directory only this user can enter. The
/// directory is removed when the plugin stops serving.
pub(crate) struct Socket {
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
