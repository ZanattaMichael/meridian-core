//! What an expression's failure is made of: a kind with a stable code, where
//! in the text it is, and the words.
//!
//! The words are what they always were, so a message read by a person is
//! unchanged; the kind and the span are for tools, which must never match on
//! wording. A code, once released, names one kind of mistake for good and is
//! never reused for another.

use std::fmt;

/// Where in an expression's text an error is: byte offsets, `start`
/// inclusive and `end` exclusive, into the text that was parsed. For an
/// expression inside an interpolation, into the whole interpolated text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }
}

/// The kind of mistake. Each has a code that tools and documentation refer to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// The text does not read as an expression.
    Syntax,
    /// The expression is too deep or too long to read safely.
    Limit,
    /// A name that nothing here is called.
    UnknownName,
    /// A name that is known but not allowed where it was written.
    RefusedName,
    /// A function call to a function that does not exist.
    UnknownFunction,
    /// A function called with the wrong number of arguments.
    Arity,
    /// A function given an argument it cannot use.
    Argument,
    /// An operator applied to values it does not work on.
    Type,
    /// A path that reads a value nothing has set.
    NotSet,
    /// A secret could not be read or combined.
    Secret,
    /// An interpolation that cannot be read or whose value cannot be placed.
    Interpolation,
    /// Anything else that stops evaluation.
    Other,
}

impl Kind {
    /// The stable code, as documentation and tools name it.
    pub fn code(self) -> &'static str {
        match self {
            Kind::Syntax => "expr-syntax",
            Kind::Limit => "expr-limit",
            Kind::UnknownName => "expr-unknown-name",
            Kind::RefusedName => "expr-refused-name",
            Kind::UnknownFunction => "expr-unknown-function",
            Kind::Arity => "expr-arity",
            Kind::Argument => "expr-argument",
            Kind::Type => "expr-type",
            Kind::NotSet => "expr-not-set",
            Kind::Secret => "expr-secret",
            Kind::Interpolation => "expr-interpolation",
            Kind::Other => "expr-other",
        }
    }

    /// Every kind, for the reference that lists them.
    pub const ALL: [Kind; 12] = [
        Kind::Syntax,
        Kind::Limit,
        Kind::UnknownName,
        Kind::RefusedName,
        Kind::UnknownFunction,
        Kind::Arity,
        Kind::Argument,
        Kind::Type,
        Kind::NotSet,
        Kind::Secret,
        Kind::Interpolation,
        Kind::Other,
    ];
}

/// Why an expression could not be read or has no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: Kind,
    /// Where in the text, when it is known.
    pub span: Option<Span>,
    /// The message, as a person reads it.
    pub msg: String,
}

impl Error {
    pub fn new(kind: Kind, msg: impl Into<String>) -> Error {
        Error {
            kind,
            span: None,
            msg: msg.into(),
        }
    }

    /// The same error, placed.
    pub fn at(mut self, span: Span) -> Error {
        self.span = Some(span);
        self
    }

    /// Placed, unless it already is: an inner error keeps its more exact
    /// place.
    pub(crate) fn or_at(mut self, span: Option<Span>) -> Error {
        if self.span.is_none() {
            self.span = span;
        }
        self
    }

    /// The same error with its span moved `by` bytes to the right, as when
    /// the expression sits inside a longer text.
    pub(crate) fn shifted(mut self, by: usize) -> Error {
        if let Some(span) = &mut self.span {
            span.start += by;
            span.end += by;
        }
        self
    }

    /// The same error with words in front of its message.
    pub(crate) fn prefixed(mut self, prefix: &str) -> Error {
        self.msg.insert_str(0, prefix);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

/// A message with no kind of its own. For the places that have only words
/// to give; everything that knows what went wrong says so.
impl From<String> for Error {
    fn from(msg: String) -> Error {
        Error::new(Kind::Other, msg)
    }
}

/// The message alone, for a caller that keeps failures as text.
impl From<Error> for String {
    fn from(error: Error) -> String {
        error.msg
    }
}
