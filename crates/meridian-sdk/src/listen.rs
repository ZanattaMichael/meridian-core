//! Where a plugin listens for its host.
//!
//! The endpoint is private to the machine and, as far as the operating system
//! allows, to the user: a Unix socket in a directory only that user can enter,
//! or on Windows a named pipe that refuses remote clients. The plugin names
//! the endpoint in its handshake line as `network|address`, and the host dials
//! exactly that.

#[cfg(unix)]
#[path = "listen/unix.rs"]
mod imp;

#[cfg(windows)]
#[path = "listen/windows.rs"]
mod imp;

pub(crate) use imp::Listening;
