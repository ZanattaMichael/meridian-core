//! The secrets of one run, by number.
//!
//! Everything outside this crate holds a secret as a [`SecretRef`]: which
//! secret, and nothing of it. The bytes are here. An expression that joins a
//! secret to text, or encodes one, asks this table to do it; the work happens
//! on protected memory and the answer is another `SecretRef`.
//!
//! A secret can be in the table before it is fetched. `validate` and
//! `compile` never fetch one, so there a secret is only its name, and what is
//! made from it is a description such as `password=(secret db_password)`.

use std::fmt;
use std::sync::Mutex;

use base64::Engine;
use meridian_value::{SecretRef, SENSITIVE};

use crate::mask::{url, url_len};
use crate::SecretValue;

/// One part of text that is being joined.
#[derive(Clone, Copy, Debug)]
pub enum Piece<'a> {
    Text(&'a str),
    Secret(SecretRef),
}

/// What can be done to a secret and give a secret. Each is done on protected
/// memory; the list is short on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    UrlEncode,
    Base64Encode,
    Base64Decode,
    Trim,
}

impl Transform {
    /// The transform a function of the expression language performs.
    pub fn of_function(name: &str) -> Option<Transform> {
        Some(match name {
            "urlEncode" => Transform::UrlEncode,
            "base64Encode" => Transform::Base64Encode,
            "base64Decode" => Transform::Base64Decode,
            "trim" => Transform::Trim,
            _ => return None,
        })
    }

    /// The functions of the expression language that take a secret.
    pub const FUNCTIONS: &'static [&'static str] =
        &["base64Decode", "base64Encode", "trim", "urlEncode"];

    fn function(self) -> &'static str {
        match self {
            Transform::UrlEncode => "urlEncode",
            Transform::Base64Encode => "base64Encode",
            Transform::Base64Decode => "base64Decode",
            Transform::Trim => "trim",
        }
    }
}

/// Why the table could not answer. No variant holds any part of a value.
#[derive(Debug, PartialEq, Eq)]
pub enum TableError {
    /// The secret has a name but has not been fetched: this is `validate` or
    /// `compile`, which never fetch one. Holds the secret's description.
    NotFetched(String),
    /// The reference names no secret of this run, or one already wiped.
    Gone,
    /// The value is not what the transform takes.
    Invalid(String),
    /// Protected memory could not be had.
    Memory(String),
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableError::NotFetched(what) => write!(
                f,
                "{what} is not fetched here: a secret is fetched by plan and apply, never by \
                 validate or compile"
            ),
            TableError::Gone => f.write_str("the secret is no longer held"),
            TableError::Invalid(why) | TableError::Memory(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for TableError {}

fn memory(e: std::io::Error) -> TableError {
    TableError::Memory(format!("a secret could not be given protected memory: {e}"))
}

enum Slot {
    /// Declared, or made from one that is only declared: what it would be.
    Described(String),
    Held(SecretValue),
    Wiped,
}

/// The secrets of one run.
#[derive(Default)]
pub struct SecretTable {
    slots: Mutex<Vec<Slot>>,
}

impl fmt::Debug for SecretTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretTable({} secrets)", self.lock().len())
    }
}

impl SecretTable {
    pub fn new() -> SecretTable {
        SecretTable::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Slot>> {
        // A panic while the lock was held leaves the slots as they were,
        // which is still a table of secrets that must be wiped on drop.
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn push(&self, slot: Slot) -> SecretRef {
        let mut slots = self.lock();
        slots.push(slot);
        SecretRef::new(slots.len() as u64 - 1)
    }

    /// Adds a secret that is known by `name` and not yet fetched.
    pub fn declare(&self, name: &str) -> SecretRef {
        self.push(Slot::Described(format!("(secret {name})")))
    }

    /// Adds a secret that is already in hand.
    pub fn insert(&self, value: SecretValue) -> SecretRef {
        self.push(Slot::Held(value))
    }

    /// Gives a declared secret its value. Returns false when `secret` names
    /// nothing in this table.
    pub fn hold(&self, secret: SecretRef, value: SecretValue) -> bool {
        match self.lock().get_mut(secret.slot() as usize) {
            Some(slot) => {
                *slot = Slot::Held(value);
                true
            }
            None => false,
        }
    }

    /// Whether the secret's value is in the table now.
    pub fn is_held(&self, secret: SecretRef) -> bool {
        matches!(self.lock().get(secret.slot() as usize), Some(Slot::Held(_)))
    }

    /// What the secret would be, when it is declared and not fetched: this
    /// is `validate` or `compile`. `None` once it is held or wiped.
    pub fn awaited(&self, secret: SecretRef) -> Option<String> {
        match self.lock().get(secret.slot() as usize) {
            Some(Slot::Described(what)) => Some(what.clone()),
            _ => None,
        }
    }

    /// Wipes one secret. Its reference stays valid and answers `Gone`.
    pub fn wipe(&self, secret: SecretRef) {
        if let Some(slot) = self.lock().get_mut(secret.slot() as usize) {
            *slot = Slot::Wiped;
        }
    }

    /// Wipes every secret. References stay valid and answer `Gone`.
    pub fn wipe_all(&self) {
        for slot in self.lock().iter_mut() {
            *slot = Slot::Wiped;
        }
    }

    /// How many secrets are held now.
    pub fn held(&self) -> usize {
        self.lock()
            .iter()
            .filter(|s| matches!(s, Slot::Held(_)))
            .count()
    }

    /// What may be written in place of the secret: `(secret name)` or the
    /// text around it for one not fetched, and `(sensitive)` for one that is.
    pub fn describe(&self, secret: SecretRef) -> String {
        match self.lock().get(secret.slot() as usize) {
            Some(Slot::Described(what)) => what.clone(),
            _ => SENSITIVE.to_owned(),
        }
    }

    /// Lends the secret itself to `f`: for the delivery channel and the
    /// output filter, which are in this crate's care.
    pub fn with<R>(
        &self,
        secret: SecretRef,
        f: impl FnOnce(&SecretValue) -> R,
    ) -> Result<R, TableError> {
        match self.lock().get(secret.slot() as usize) {
            Some(Slot::Held(value)) => Ok(f(value)),
            Some(Slot::Described(what)) => Err(TableError::NotFetched(what.clone())),
            Some(Slot::Wiped) | None => Err(TableError::Gone),
        }
    }

    /// Joins text and secrets into one new secret.
    pub fn join(&self, pieces: &[Piece<'_>]) -> Result<SecretRef, TableError> {
        let made = {
            let slots = self.lock();
            let mut len = 0usize;
            let mut described = false;
            for piece in pieces {
                match piece {
                    Piece::Text(text) => len += text.len(),
                    Piece::Secret(secret) => match slots.get(secret.slot() as usize) {
                        Some(Slot::Held(value)) => len += value.len(),
                        Some(Slot::Described(_)) => described = true,
                        Some(Slot::Wiped) | None => return Err(TableError::Gone),
                    },
                }
            }
            if described {
                // Nothing is fetched, so nothing is secret yet: say what the
                // text would be. A fetched secret beside an unfetched one is
                // still not written.
                let mut what = String::new();
                for piece in pieces {
                    match piece {
                        Piece::Text(text) => what.push_str(text),
                        Piece::Secret(secret) => match slots.get(secret.slot() as usize) {
                            Some(Slot::Described(part)) => what.push_str(part),
                            _ => what.push_str(SENSITIVE),
                        },
                    }
                }
                Slot::Described(what)
            } else {
                let joined = SecretValue::filled(len, |home| {
                    let mut at = 0;
                    for piece in pieces {
                        match piece {
                            Piece::Text(text) => {
                                home[at..at + text.len()].copy_from_slice(text.as_bytes());
                                at += text.len();
                            }
                            Piece::Secret(secret) => {
                                if let Some(Slot::Held(value)) = slots.get(secret.slot() as usize) {
                                    value.expose(|bytes| {
                                        home[at..at + bytes.len()].copy_from_slice(bytes);
                                    });
                                    at += value.len();
                                }
                            }
                        }
                    }
                    Ok(())
                })
                .map_err(memory)?;
                Slot::Held(joined)
            }
        };
        Ok(self.push(made))
    }

    /// A new secret made from `secret` by `transform`.
    pub fn transform(
        &self,
        secret: SecretRef,
        transform: Transform,
    ) -> Result<SecretRef, TableError> {
        let made = match self.lock().get(secret.slot() as usize) {
            Some(Slot::Described(what)) => {
                Slot::Described(format!("{}({what})", transform.function()))
            }
            Some(Slot::Held(value)) => Slot::Held(apply(value, transform)?),
            Some(Slot::Wiped) | None => return Err(TableError::Gone),
        };
        Ok(self.push(made))
    }

    /// Whether two secrets hold the same bytes.
    pub fn same(&self, a: SecretRef, b: SecretRef) -> Result<bool, TableError> {
        let slots = self.lock();
        match (slots.get(a.slot() as usize), slots.get(b.slot() as usize)) {
            (Some(Slot::Held(a)), Some(Slot::Held(b))) => Ok(a.same(b)),
            (Some(Slot::Described(what)), _) | (_, Some(Slot::Described(what))) => {
                Err(TableError::NotFetched(what.clone()))
            }
            _ => Err(TableError::Gone),
        }
    }

    /// Whether the secret is exactly `text`.
    pub fn same_as_text(&self, secret: SecretRef, text: &str) -> Result<bool, TableError> {
        // The text is not secret, but the comparison must still take the
        // same time wherever the two differ, so it is made between two
        // protected values.
        let other = SecretValue::filled(text.len(), |home| {
            home.copy_from_slice(text.as_bytes());
            Ok(())
        })
        .map_err(memory)?;
        self.with(secret, |value| value.same(&other))
    }

    /// Whether the secret is no bytes at all.
    pub fn is_empty(&self, secret: SecretRef) -> Result<bool, TableError> {
        self.with(secret, SecretValue::is_empty)
    }
}

fn apply(value: &SecretValue, transform: Transform) -> Result<SecretValue, TableError> {
    let engine = base64::engine::general_purpose::STANDARD;
    match transform {
        Transform::UrlEncode => {
            let len = value.expose(url_len);
            value.derive(len, url).map_err(memory)
        }
        Transform::Base64Encode => {
            let len = value.len().div_ceil(3) * 4;
            value
                .derive(len, |from, home| {
                    // The length is exact, so this cannot fail.
                    let _ = engine.encode_slice(from, home);
                })
                .map_err(memory)
        }
        Transform::Base64Decode => {
            // Decode into protected scratch space, then keep what was read.
            let mut read = None;
            let scratch = value
                .derive(value.len() / 4 * 3 + 3, |from, home| {
                    read = engine.decode_slice(from, home).ok();
                })
                .map_err(memory)?;
            // The error says nothing of where the value stopped being
            // base64, which would be something about the value.
            let read = read.ok_or_else(|| {
                TableError::Invalid("base64Decode(...): the secret is not base64".to_owned())
            })?;
            scratch
                .derive(read, |from, home| home.copy_from_slice(&from[..read]))
                .map_err(memory)
        }
        Transform::Trim => {
            let (start, end) = value.expose(|bytes| {
                let start = bytes
                    .iter()
                    .position(|b| !b.is_ascii_whitespace())
                    .unwrap_or(bytes.len());
                let end = bytes
                    .iter()
                    .rposition(|b| !b.is_ascii_whitespace())
                    .map_or(start, |last| last + 1);
                (start, end)
            });
            value
                .derive(end - start, |from, home| {
                    home.copy_from_slice(&from[start..end]);
                })
                .map_err(memory)
        }
    }
}
