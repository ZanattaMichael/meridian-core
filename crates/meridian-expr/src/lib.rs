//! Meridian's expression language.
//!
//! There is one expression language, and this crate is the only place it is
//! read or decided. A `when` condition and every interpolation are the same
//! grammar, evaluated by the same code, with the same errors, so a condition
//! cannot mean one thing for one target and another for the next: no target
//! ever sees one.
//!
//! Three properties hold for every expression, and the tests assert them:
//!
//! - **It always ends.** There are no loops and no recursion. The two
//!   functions that take an expression, `map` and `filter`, are bounded by the
//!   list they are given.
//! - **It is pure.** The same expression over the same [`Scope`] gives the
//!   same value. The one window to the outside, `env()`, reads the scope's
//!   own copy of the environment when it has one.
//! - **It never guesses.** A name that is not set, two types that do not go
//!   together and a value that is not a yes or a no are errors, not `false`
//!   and not an empty string.

pub mod error;
mod eval;
mod funcs;
mod parse;
mod template;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use meridian_secret::SecretTable;

use meridian_value::Value;

pub use error::{Error, Kind, Span};
pub use funcs::{function, function_names, functions, FunctionInfo};
use parse::Node;
pub use template::{has_interpolation, Template};

/// A parsed expression. Parsing checks the grammar; [`Expr::check`] checks
/// the names against what a place allows; [`Expr::eval`] decides it.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    node: Node,
}

/// The names an expression may read in the place it is written, known
/// without any data. This is what `meridian validate` can check.
#[derive(Clone, Debug, Default)]
pub struct Names {
    roots: BTreeSet<String>,
    refused: BTreeMap<String, String>,
    hint: String,
}

impl Names {
    /// Nothing is readable.
    pub fn new() -> Self {
        Names::default()
    }

    /// Allows the root `name`, as in `data`.
    pub fn allow(mut self, name: &str) -> Self {
        self.roots.insert(name.to_owned());
        self
    }

    /// Refuses the root `name` with a reason of its own, for a name that is
    /// part of the language but not available here.
    pub fn refuse(mut self, name: &str, why: &str) -> Self {
        self.refused.insert(name.to_owned(), why.to_owned());
        self
    }

    /// What to say after "unknown name": what this place can read.
    pub fn hint(mut self, hint: &str) -> Self {
        hint.clone_into(&mut self.hint);
        self
    }
}

/// What an expression is decided against: a value for each root name.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    roots: BTreeMap<String, Value>,
    unset_hints: BTreeMap<String, String>,
    env: Option<BTreeMap<String, String>>,
    secrets: Option<Arc<SecretTable>>,
}

impl Scope {
    pub fn new() -> Self {
        Scope::default()
    }

    /// Sets the root `name`, as in `data`, to `value`.
    pub fn with(mut self, name: &str, value: Value) -> Self {
        self.roots.insert(name.to_owned(), value);
        self
    }

    /// What to suggest when a path under `root` is not set. `{path}` in the
    /// hint is replaced with the path that was read.
    pub fn with_unset_hint(mut self, root: &str, hint: &str) -> Self {
        self.unset_hints.insert(root.to_owned(), hint.to_owned());
        self
    }

    /// The run's secrets. A value that is a secret names one of them, and an
    /// expression that joins, encodes or compares a secret has the table do
    /// it, so the expression itself never holds a secret's bytes.
    pub fn with_secrets(mut self, secrets: Arc<SecretTable>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// Gives `env()` a fixed environment to read instead of the process's
    /// own, so a result does not depend on where it was computed.
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = Some(env);
        self
    }
}

impl Expr {
    /// Reads a `when` condition. Diagnostics call it "the condition".
    pub fn parse_condition(text: &str) -> Result<Expr, Error> {
        Ok(Expr {
            node: parse::parse(text, "condition")?,
        })
    }

    /// Reads an expression. Diagnostics call it "the expression".
    pub fn parse(text: &str) -> Result<Expr, Error> {
        Ok(Expr {
            node: parse::parse(text, "expression")?,
        })
    }

    /// Checks everything that can be known without data: every name is one
    /// this place can read, every function exists and is given a number of
    /// arguments it takes.
    pub fn check(&self, names: &Names) -> Result<(), Error> {
        eval::check(&self.node, names, &mut Vec::new())
    }

    /// The root names the expression reads, such as `data` and `param`.
    pub fn roots(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        eval::roots(&self.node, &mut Vec::new(), &mut out);
        out
    }

    /// Every root the expression reads, with the first key under it when
    /// that key is written out: `var.name` is `("var", Some("name"))`. This
    /// is how the order of variables and named conditions is found.
    pub fn reads(&self) -> BTreeSet<(String, Option<String>)> {
        let mut out = BTreeSet::new();
        eval::reads(&self.node, &mut Vec::new(), &mut out);
        out
    }

    /// Decides the expression.
    pub fn eval(&self, scope: &Scope) -> Result<Value, Error> {
        eval::Evaluator::new(scope)
            .value(&self.node)
            .map_err(eval::Failure::into_error)
    }

    /// Decides the expression as a yes or a no. Only a boolean is one: text,
    /// a number or null deciding whether something exists would be a guess.
    pub fn truth(&self, scope: &Scope) -> Result<bool, Error> {
        eval::Evaluator::new(scope)
            .truth(&self.node)
            .map_err(eval::Failure::into_error)
    }
}
