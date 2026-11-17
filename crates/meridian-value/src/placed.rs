//! How a target keeps text that Meridian placed from data out of its tool's
//! own expression syntax.
//!
//! The core promises that a value an interpolation places is literal text to
//! the tool: what the author wrote in the document can be an expression, what
//! the data supplied never becomes one. For Meridian's own `${` the core
//! keeps that promise alone, by handing a placed `${` over as `$${`. A tool
//! may have expression syntax of its own, which only its plugin knows: Jinja's
//! `{{` for Ansible, a leading `[` for an ARM template. A plugin declares
//! those here, and the core applies them to every placed value before the
//! plugin sees it, so the plugin never has to tell placed text from written
//! text.

/// One opening of a tool's expression syntax, and how the tool writes it as
/// literal text.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PlacedEscape {
    /// The text that opens an expression for the tool, such as `{{`.
    pub text: String,
    /// What the tool reads as that text and nothing more, such as
    /// `{{ '{{' }}`.
    pub written: String,
    /// Whether the text opens an expression only at the start of a value, as
    /// `[` does in an ARM template. Otherwise it does so anywhere.
    pub only_at_start: bool,
}

impl PlacedEscape {
    /// An opening that means an expression anywhere in a value.
    pub fn anywhere(text: &str, written: &str) -> Self {
        PlacedEscape {
            text: text.to_owned(),
            written: written.to_owned(),
            only_at_start: false,
        }
    }

    /// An opening that means an expression only at the start of a value.
    pub fn at_start(text: &str, written: &str) -> Self {
        PlacedEscape {
            text: text.to_owned(),
            written: written.to_owned(),
            only_at_start: true,
        }
    }
}
