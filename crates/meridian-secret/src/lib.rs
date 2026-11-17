//! Meridian's secrets, in memory and on their way to a tool.
//!
//! A secret value has one home in the process: a [`SecretValue`]. It
//!
//! - **cannot print itself.** `Debug` and `Display` give `(sensitive)`, and
//!   it is not `Clone`, `Serialize`, `PartialEq` or `Deref`, so the ways a
//!   value usually ends up in a log, a panic message or a file do not
//!   compile, or print the mask;
//! - **is wiped when it is dropped**, with a write the compiler cannot
//!   remove;
//! - **never moves or grows**, so no earlier copy of it is left behind in
//!   freed memory;
//! - **is kept out of swap and out of core dumps**, between two guard pages,
//!   where the operating system allows ([`Protection`] says what it allowed).
//!
//! The bytes are reached in exactly one way, [`SecretValue::expose`], which
//! lends them to a closure. Every call to it is a place a secret can leave
//! this crate's protection, so there are few, and a test in this crate fails
//! when one appears anywhere it is not expected.
//!
//! The honest limit: none of this protects a secret from someone who can
//! read the live memory of the process, such as root or a debugger attached
//! before [`harden_process`] ran. What it does is make sure a secret is in
//! memory only while it is used, in one place, and nowhere afterwards.
//!
//! ```
//! use meridian_secret::SecretValue;
//!
//! let secret = SecretValue::from_reader(&b"hunter2"[..], 64).unwrap();
//! assert_eq!(format!("{secret:?} {secret}"), "(sensitive) (sensitive)");
//! assert_eq!(secret.len(), 7);
//! assert!(secret.expose(|bytes| bytes == b"hunter2"));
//! ```
//!
//! A secret cannot be copied by accident, written out, or compared with `==`:
//!
//! ```compile_fail
//! let secret = meridian_secret::SecretValue::from_reader(&b"x"[..], 8).unwrap();
//! let copy = secret.clone();
//! ```
//!
//! ```compile_fail
//! let secret = meridian_secret::SecretValue::from_reader(&b"x"[..], 8).unwrap();
//! let text: &[u8] = &secret;
//! ```
//!
//! ```compile_fail
//! let a = meridian_secret::SecretValue::from_reader(&b"x"[..], 8).unwrap();
//! let b = meridian_secret::SecretValue::from_reader(&b"x"[..], 8).unwrap();
//! let same = a == b;
//! ```

mod credential;
mod deliver;
mod mask;
mod provider;
mod sys;
mod table;

use std::fmt;
use std::io::Read;

pub use credential::{
    resolve_credential, Credential, CredentialDeclaration, CredentialError, CredentialField,
    FieldSource, CREDENTIAL_TYPES,
};
pub use deliver::{sweep_stale, Part, SecretFile};
pub use mask::Masker;
pub use provider::{
    from_env, from_exec, from_file, FetchError, FileOptions, Probe, Source, PROVIDERS,
};
pub use sys::Protection;
pub use table::{Piece, SecretTable, TableError, Transform};

/// What is shown wherever a secret would be.
pub const MASK: &str = "(sensitive)";

/// The largest secret a provider will read unless it is told otherwise: big
/// enough for a kubeconfig or a certificate chain, small enough that a
/// provider pointed at the wrong file does not lock the machine's memory.
pub const DEFAULT_MAX_LEN: usize = 256 * 1024;

/// A secret value. See the [crate documentation](crate).
pub struct SecretValue {
    region: sys::Region,
}

impl SecretValue {
    /// A secret of exactly `len` bytes, filled in place by `fill`, so the
    /// value never exists anywhere but its final home.
    pub fn filled(
        len: usize,
        fill: impl FnOnce(&mut [u8]) -> std::io::Result<()>,
    ) -> std::io::Result<SecretValue> {
        let mut region = sys::Region::new(len)?;
        fill(region.bytes_mut())?;
        Ok(SecretValue { region })
    }

    /// Reads a secret from `reader` until it ends. Nothing is buffered on
    /// the way: the bytes go from the reader into protected memory. A value
    /// longer than `max` bytes is refused, so a provider pointed at the
    /// wrong thing fails instead of filling memory.
    pub fn from_reader(mut reader: impl Read, max: usize) -> std::io::Result<SecretValue> {
        // Read into protected scratch space of the largest size allowed,
        // then keep a copy of exactly the size read. The scratch space is
        // wiped when it is dropped at the end of this function.
        let mut scratch = sys::Region::new(max.saturating_add(1))?;
        let mut read = 0;
        loop {
            let space = &mut scratch.bytes_mut()[read..];
            if space.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("the value is longer than the {max} bytes a secret may be"),
                ));
            }
            match reader.read(space) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        SecretValue::filled(read, |bytes| {
            bytes.copy_from_slice(&scratch.bytes()[..read]);
            Ok(())
        })
    }

    /// Takes a secret out of memory this crate does not control, and wipes
    /// that memory. Use it at the one point a value arrives from somewhere
    /// that could only hand over ordinary bytes.
    pub fn take_from(bytes: &mut [u8]) -> std::io::Result<SecretValue> {
        let secret = SecretValue::filled(bytes.len(), |home| {
            home.copy_from_slice(bytes);
            Ok(())
        });
        sys::wipe(bytes);
        secret
    }

    /// Lends the bytes to `f`. This is the only way to reach them.
    ///
    /// Whatever `f` copies out of the slice has left this type's protection,
    /// so `f` should hand the bytes straight to where they are going (a
    /// pipe, a file made by [`SecretFile`]) and return nothing made of them.
    pub fn expose<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(self.region.bytes())
    }

    /// How many bytes the secret is. The length is not secret.
    pub fn len(&self) -> usize {
        self.region.bytes().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A second, separately protected copy. A copy is never implicit.
    pub fn duplicate(&self) -> std::io::Result<SecretValue> {
        SecretValue::filled(self.len(), |home| {
            home.copy_from_slice(self.region.bytes());
            Ok(())
        })
    }

    /// A new secret made from this one by `f`, which writes into protected
    /// memory of `len` bytes: how an encoded or joined form of a secret is
    /// made without the bytes passing through ordinary memory.
    pub fn derive(
        &self,
        len: usize,
        f: impl FnOnce(&[u8], &mut [u8]),
    ) -> std::io::Result<SecretValue> {
        SecretValue::filled(len, |home| {
            f(self.region.bytes(), home);
            Ok(())
        })
    }

    /// Whether two secrets hold the same bytes, in time that does not depend
    /// on where they first differ.
    pub fn same(&self, other: &SecretValue) -> bool {
        let (a, b) = (self.region.bytes(), other.region.bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut difference = 0u8;
        for (x, y) in a.iter().zip(b) {
            difference |= x ^ y;
        }
        std::hint::black_box(difference) == 0
    }

    /// The secret without one trailing line ending, which a command or a
    /// file written by an editor usually adds and the secret does not have.
    pub fn without_trailing_newline(self) -> std::io::Result<SecretValue> {
        let bytes = self.region.bytes();
        let keep = match bytes {
            [rest @ .., b'\r', b'\n'] | [rest @ .., b'\n'] => rest.len(),
            _ => return Ok(self),
        };
        self.derive(keep, |from, home| home.copy_from_slice(&from[..keep]))
    }

    /// What the operating system agreed to do for this secret's memory.
    pub fn protection(&self) -> Protection {
        self.region.protection()
    }

    /// Where the bytes are. Only for tests that inspect the process's own
    /// memory map; nothing else has a use for an address.
    #[doc(hidden)]
    pub fn address_for_tests(&self) -> usize {
        self.region.bytes().as_ptr() as usize
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

/// What [`harden_process`] achieved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hardening {
    /// A crash of this process writes no core file.
    pub core_dumps_off: bool,
    /// Another process of the same user cannot attach a debugger to this one
    /// or read its memory, and the kernel will not dump it.
    pub undumpable: bool,
}

/// Makes the process a worse place to steal a secret from: core dumps are
/// turned off and, where the system has the notion, the process is made
/// undumpable. Call it once, at start, before any secret is read.
///
/// It is not an error for the system to refuse; the answer says what was
/// achieved, and a caller that must have both checks it.
pub fn harden_process() -> Hardening {
    let (core_dumps_off, undumpable) = sys::harden();
    Hardening {
        core_dumps_off,
        undumpable,
    }
}

/// What the system reports now, whoever set it.
pub fn hardening() -> Hardening {
    let (core_dumps_off, undumpable) = sys::hardened();
    Hardening {
        core_dumps_off,
        undumpable,
    }
}
