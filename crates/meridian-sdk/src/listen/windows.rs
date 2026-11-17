use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::server::Connected;

use crate::ServeError;

/// A plugin's listening endpoint: what to announce, and the connections that
/// arrive on it.
pub(crate) struct Listening {
    /// What the handshake line gives as the address to dial.
    pub address: String,
    pub incoming: ReceiverStream<io::Result<PipeConnection>>,
    /// Nothing to clean up: a named pipe is gone when its last handle closes.
    pub _guard: (),
}

impl Listening {
    /// The handshake's name for this kind of endpoint.
    pub const NETWORK: &'static str = "npipe";

    /// Must be called on the plugin's runtime: it starts the task that
    /// accepts connections.
    pub fn start() -> Result<Listening, ServeError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let mut last = String::from("no pipe name was free");
        for attempt in 0..16u32 {
            let name = format!(
                r"\\.\pipe\meridian-plugin-{}-{:08x}",
                std::process::id(),
                nanos.wrapping_add(attempt)
            );
            // The name only has to be unused. Creating the first instance
            // fails if a pipe of that name already exists, so a guessed name
            // cannot be used to stand between the plugin and its host.
            //
            // The pipe has Windows' default access: this user, administrators
            // and the system may write to it, and every local account may
            // open it for reading. A request needs writing, so another
            // account cannot ask the plugin for anything; what it can do is
            // open the pipe and hold a connection that says nothing.
            // Narrowing that to this user alone takes an explicit security
            // descriptor, which needs `unsafe` code this workspace forbids.
            match first_instance(&name) {
                Ok(server) => {
                    let (connections, incoming) = mpsc::channel(16);
                    tokio::spawn(accept(name.clone(), server, connections));
                    return Ok(Listening {
                        address: name,
                        incoming: ReceiverStream::new(incoming),
                        _guard: (),
                    });
                }
                Err(e) => last = format!("{name}: {e}"),
            }
        }
        Err(ServeError::new("creating the plugin's named pipe", last))
    }
}

fn first_instance(name: &str) -> io::Result<NamedPipeServer> {
    ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(name)
}

/// Hands each connection to the server, keeping one instance of the pipe
/// waiting at all times so a client never finds the name gone.
async fn accept(
    name: String,
    mut server: NamedPipeServer,
    connections: mpsc::Sender<io::Result<PipeConnection>>,
) {
    loop {
        if server.connect().await.is_err() {
            return;
        }
        // The next instance is created before this one is handed over, so
        // there is no moment at which the pipe does not exist.
        let next = match ServerOptions::new()
            .reject_remote_clients(true)
            .create(&name)
        {
            Ok(next) => next,
            Err(e) => {
                let _ = connections.send(Err(e)).await;
                return;
            }
        };
        let connected = std::mem::replace(&mut server, next);
        if connections
            .send(Ok(PipeConnection(connected)))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// One host connection over the named pipe.
pub(crate) struct PipeConnection(NamedPipeServer);

impl Connected for PipeConnection {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for PipeConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for PipeConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
