//! Importing: turning a tool's existing configuration into Meridian documents.
//!
//! It is the reverse of [`crate::Emitter`], and it is optional. A target that
//! implements [`Importer`] owns the knowledge of how its tool's files map back
//! onto the documents it compiles, which is the only place that knowledge can
//! live without the core learning about products.
//!
//! An importer returns documents as data, not as text. The host writes them
//! out in the language's current syntax, so an importer keeps working when the
//! way a document is spelled changes.
//!
//! The rule an importer must keep is the one a compile keeps, in reverse:
//! nothing is lost silently. Whatever the source says that the imported
//! document does not, or says differently, is reported as an [`ImportNote`],
//! and the host turns every such note into a `TODO` marker in the document it
//! writes.

use std::collections::BTreeMap;
use std::fmt;

use meridian_value::Value;

/// One kind of existing configuration a plugin can read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportFormat {
    /// A short word for the format, such as `hcl` or `compose`.
    pub name: String,
    /// One line a person can read in a listing.
    pub description: String,
    /// The file names the format usually has, as globs over a file's base
    /// name: `*.tf`, `compose.yaml`. `*` matches any run of characters and
    /// `?` any one. The host uses them to gather a directory's files and to
    /// guess which plugin a file belongs to; they are a hint, and the
    /// importer still decides what it can read.
    pub patterns: Vec<String>,
}

impl ImportFormat {
    pub fn new(name: &str, description: &str, patterns: &[&str]) -> Self {
        ImportFormat {
            name: name.to_owned(),
            description: description.to_owned(),
            patterns: patterns.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    /// Whether a file's base name matches one of the format's patterns.
    pub fn matches(&self, file_name: &str) -> bool {
        self.matching(file_name).next().is_some()
    }

    /// The patterns of the format that a file's base name matches.
    pub fn matching<'a>(&'a self, file_name: &'a str) -> impl Iterator<Item = &'a str> {
        self.patterns
            .iter()
            .map(String::as_str)
            .filter(move |p| glob(p, file_name))
    }
}

/// Matches a name against a pattern in which `*` is any run of characters and
/// `?` is any one character.
fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// The configuration to import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSource {
    /// Path, relative to what the operator named, to the file's text. Ordered,
    /// so an import is deterministic.
    pub files: BTreeMap<String, String>,
    /// What the operator wants the imported document called; empty lets the
    /// importer choose. When the source yields several documents the importer
    /// derives each one's name from it.
    pub name: String,
    /// The directory the files were read from, as an absolute path on the
    /// operator's machine; empty when they did not come from one place. An
    /// importer never reads it. It is there for a configuration whose meaning
    /// depends on where it is, such as one that names files beside itself.
    pub root: String,
}

/// Which of Meridian's document kinds an imported document is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DocumentKind {
    /// Provisions things.
    #[default]
    Infrastructure,
    /// Configures a host that exists.
    ResourceSet,
}

impl DocumentKind {
    /// The word a document's `kind` field holds.
    pub fn as_str(self) -> &'static str {
        match self {
            DocumentKind::Infrastructure => "Infrastructure",
            DocumentKind::ResourceSet => "ResourceSet",
        }
    }
}

/// A notification an imported resource sends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportedNotify {
    pub resource: String,
    pub action: String,
}

/// One resource of an imported document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedResource {
    pub id: String,
    pub r#type: String,
    /// Empty when the document should not write one.
    pub state: String,
    pub params: BTreeMap<String, Value>,
    /// The ids of the resources this one must follow.
    pub depends_on: Vec<String>,
    pub notifies: Vec<ImportedNotify>,
    pub runtime_when: String,
    /// Where in the source the resource came from, such as `main.tf:12`. It
    /// is written beside the resource as a comment.
    pub origin: String,
}

/// A value an imported document promises to later stages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportedOutput {
    pub name: String,
    pub value: String,
}

/// One Meridian document, as data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedDocument {
    pub kind: DocumentKind,
    pub name: String,
    /// The target the document selects. Empty means the plugin that imported
    /// it, and the host fills it in.
    pub target: String,
    pub target_version: String,
    /// The node a ResourceSet configures; empty when the source names none.
    pub host: String,
    pub resources: Vec<ImportedResource>,
    pub outputs: Vec<ImportedOutput>,
}

/// How much a note should worry the operator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NoteLevel {
    /// Carried over faithfully; the note explains how.
    #[default]
    Info,
    /// Carried over, but not as the source wrote it. The document may behave
    /// differently and should be reviewed.
    Changed,
    /// Not carried over at all. The document does less than the source did
    /// until someone writes the missing part.
    Untranslated,
}

impl NoteLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            NoteLevel::Info => "note",
            NoteLevel::Changed => "changed",
            NoteLevel::Untranslated => "not translated",
        }
    }

    /// Whether the note leaves the operator something to do, and so becomes a
    /// `TODO` marker in the written document.
    pub fn needs_attention(self) -> bool {
        self != NoteLevel::Info
    }
}

impl fmt::Display for NoteLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing an importer wants the operator to know.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportNote {
    pub level: NoteLevel,
    /// Where in the source the note is about, such as `main.tf:12`.
    pub origin: String,
    /// The thing, in the source's own terms: `variable "region"`.
    pub subject: String,
    pub msg: String,
    /// The imported document the note belongs to; empty when it is about the
    /// import as a whole.
    pub document: String,
    /// The imported resource the note belongs to; empty when it is about the
    /// document.
    pub resource: String,
}

impl fmt::Display for ImportNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.origin.is_empty() {
            write!(f, "{}: ", self.origin)?;
        }
        if !self.subject.is_empty() {
            write!(f, "{}: ", self.subject)?;
        }
        f.write_str(&self.msg)
    }
}

/// What an import produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Imported {
    pub documents: Vec<ImportedDocument>,
    pub notes: Vec<ImportNote>,
}

impl Imported {
    /// How many notes leave the operator something to do.
    pub fn needing_attention(&self) -> usize {
        self.notes
            .iter()
            .filter(|n| n.level.needs_attention())
            .count()
    }
}

/// An importer being unable to read the source at all: a syntax error, or a
/// file that is not its tool's.
///
/// It is not a fault. Like a [`crate::CompileError`] it is an answer about
/// what the operator handed over, so it crosses the process boundary as part
/// of a successful response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportError {
    pub msg: String,
    /// The file the problem is in; empty when it is not about one.
    pub file: String,
    /// 1-based; zero when unknown.
    pub line: usize,
}

impl ImportError {
    pub fn new(msg: impl Into<String>) -> Self {
        ImportError {
            msg: msg.into(),
            ..ImportError::default()
        }
    }

    pub fn at(file: &str, line: usize, msg: impl Into<String>) -> Self {
        ImportError {
            msg: msg.into(),
            file: file.to_owned(),
            line,
        }
    }
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.file.is_empty() {
            f.write_str(&self.file)?;
            if self.line > 0 {
                write!(f, ":{}", self.line)?;
            }
            f.write_str(": ")?;
        }
        f.write_str(&self.msg)
    }
}

impl std::error::Error for ImportError {}

/// Turns a tool's existing configuration into Meridian documents.
///
/// It has no side effects: it reads the text it is given and nothing else. It
/// never runs the tool, and never reads the tool's state.
pub trait Importer: Send + Sync {
    /// The kinds of configuration this importer reads.
    fn formats(&self) -> Vec<ImportFormat>;

    /// Reads the source, or says why it cannot be read.
    fn import(&self, source: &ImportSource) -> Result<Imported, ImportError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_matches_a_whole_file_name() {
        for (pattern, name, want) in [
            ("*.tf", "main.tf", true),
            ("*.tf", "main.tf.json", false),
            ("*.tf.json", "main.tf.json", true),
            ("compose.y*ml", "compose.yaml", true),
            ("compose.y*ml", "compose.yml", true),
            ("compose.y*ml", "docker-compose.yml", false),
            ("*compose*.y?ml", "docker-compose.prod.yaml", true),
            ("*", "anything", true),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("", "", true),
            ("", "x", false),
        ] {
            assert_eq!(glob(pattern, name), want, "{pattern} against {name}");
        }
    }

    #[test]
    fn an_error_names_its_file_and_line_when_it_has_them() {
        assert_eq!(
            ImportError::at("main.tf", 3, "unexpected }").to_string(),
            "main.tf:3: unexpected }"
        );
        assert_eq!(
            ImportError::new("nothing to read").to_string(),
            "nothing to read"
        );
    }
}
