//! Reading a `Test` document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use meridian_expr::{Expr, Names};
use meridian_ir::Data;
use meridian_value::Value;

/// The `kind` of a test document.
pub const TEST_KIND: &str = "Test";

/// What a test file is called, so that `meridian test` with no files finds
/// them, and `meridian lint` over a directory leaves them to it.
pub const TEST_FILE_SUFFIX: &str = ".test.yaml";

/// One `Test` document, as read from a file.
#[derive(Clone, Debug, PartialEq)]
pub struct TestFile {
    /// The file it was read from.
    pub path: PathBuf,
    /// `metadata.name`.
    pub name: String,
    /// `spec.documents`, resolved against the file's own directory. A
    /// module reference (`@acme/x:documents/y.yaml`) is kept as written.
    pub documents: Vec<PathBuf>,
    /// `spec.data`, resolved the same way.
    pub data: Vec<PathBuf>,
    /// `spec.set`: values merged over the data for every case.
    pub set: Data,
    /// `spec.select`: a label selector every case runs under.
    pub select: Option<String>,
    /// `spec.cases`, in order.
    pub cases: Vec<Case>,
}

/// How far a case runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// Compile each document and look at the files. Needs no tool.
    Compile,
    /// Compile, then ask each tool what it would change, from an empty
    /// state. Needs the tool, or a stand-in.
    Plan,
    /// Apply to a scratch environment, check it, then destroy it. Needs the
    /// tool and something to apply to.
    Lifecycle,
}

impl Run {
    pub fn as_str(self) -> &'static str {
        match self {
            Run::Compile => "compile",
            Run::Plan => "plan",
            Run::Lifecycle => "lifecycle",
        }
    }
}

/// One case of a test.
#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    pub name: String,
    pub run: Run,
    /// `data`: files read after the document's `spec.data`.
    pub data: Vec<PathBuf>,
    /// `set`: values merged over the data after the document's `spec.set`.
    pub set: Data,
    /// `select`: a label selector, in place of the document's.
    pub select: Option<String>,
    /// `expect`: what each named document should have come to.
    pub expect: BTreeMap<String, Expect>,
    /// `assert`: expressions that must each be true.
    pub assert: Vec<Assertion>,
    /// `phases`, for a lifecycle case.
    pub phases: Phases,
    /// `timeout`, for a lifecycle case: how long provision and verify may
    /// take together. Destroy runs after it, with the same limit.
    pub timeout: Option<Duration>,
}

/// The phases of a lifecycle case. Provision and destroy always run; the
/// checks of `expect` and `assert` are made in verify.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Phases {
    /// `verify.drift: none`: a second plan after provision must change
    /// nothing.
    pub no_drift: bool,
}

/// What one document of a case is expected to come to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Expect {
    /// `compiled`, `refused`, `left-out`, `planned`, `deferred` or
    /// `failed`.
    pub outcome: Option<String>,
    /// For a refusal, the rule it must carry.
    pub rule: Option<String>,
    /// For a refusal, the resource it must name.
    pub resource: Option<String>,
    /// Text the refusal's message must hold.
    pub message: Option<String>,
    /// For a compile, what each emitted file must hold, by path.
    pub files: BTreeMap<String, FileExpect>,
    /// Warnings the compile must raise, each as text the warning holds.
    pub warnings: Vec<String>,
    /// For a plan, changes that must be among the tool's.
    pub changes: Vec<ChangeExpect>,
    /// For a plan, exactly how many changes the tool reports.
    pub change_count: Option<usize>,
}

/// What an emitted file must hold.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileExpect {
    pub contains: Vec<String>,
    pub not_contains: Vec<String>,
    pub equals: Option<String>,
    pub matches: Vec<String>,
}

/// A change a plan must report: the action, the resource, or both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChangeExpect {
    pub action: Option<String>,
    pub resource: Option<String>,
}

/// An expression that must be true.
#[derive(Clone, Debug, PartialEq)]
pub struct Assertion {
    /// The expression as written.
    pub text: String,
    pub expr: Expr,
    /// What to say when it is false, instead of the expression.
    pub message: Option<String>,
}

/// The names an assertion may read.
pub(crate) fn names() -> Names {
    Names::new()
        .allow("stages")
        .allow("data")
        .hint("an assertion can read stages.<document> and data")
}

/// Every `*.test.yaml` under `tests/` in `dir`, sorted; empty when there is
/// no such directory.
pub fn discover(dir: &Path) -> Vec<PathBuf> {
    // `tests/`, not `./tests/`, when the directory is the current one.
    let tests = if dir == Path::new(".") {
        PathBuf::from("tests")
    } else {
        dir.join("tests")
    };
    let Ok(entries) = std::fs::read_dir(&tests) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(TEST_FILE_SUFFIX))
        })
        .collect();
    found.sort();
    found
}

/// Reads every `Test` document in the file. The first problem is the
/// error, named by where it is in the document.
pub fn load(path: &Path) -> Result<Vec<TestFile>, String> {
    let shown = path.display();
    let bytes = std::fs::read(path).map_err(|e| format!("{shown}: cannot read file: {e}"))?;
    let roots = meridian_ir::parse_stream(&shown.to_string(), &bytes).map_err(|e| e.to_string())?;
    if roots.is_empty() {
        return Err(format!("{shown}: no documents found"));
    }
    let base = path.parent().unwrap_or(Path::new("."));
    roots
        .iter()
        .enumerate()
        .map(|(i, root)| {
            let at = if roots.len() == 1 {
                shown.to_string()
            } else {
                format!("{shown} (document {})", i + 1)
            };
            read(root, &at, base, path)
        })
        .collect()
}

/// A reader that names where a problem is.
struct Reader<'a> {
    at: &'a str,
    base: &'a Path,
}

impl Reader<'_> {
    fn refuse(&self, path: &str, why: impl std::fmt::Display) -> String {
        format!("{}: {path}: {why}", self.at)
    }

    fn map<'v>(&self, value: &'v Value, path: &str) -> Result<&'v BTreeMap<String, Value>, String> {
        value.as_map().ok_or_else(|| {
            self.refuse(
                path,
                format!("expected a mapping, found {}", value.describe()),
            )
        })
    }

    fn text(&self, value: &Value, path: &str) -> Result<String, String> {
        match value {
            Value::String(s) => Ok(s.clone()),
            other => Err(self.refuse(path, format!("expected text, found {}", other.describe()))),
        }
    }

    fn texts(&self, value: &Value, path: &str) -> Result<Vec<String>, String> {
        match value {
            Value::String(s) => Ok(vec![s.clone()]),
            Value::List(items) => items
                .iter()
                .enumerate()
                .map(|(i, item)| self.text(item, &format!("{path}[{i}]")))
                .collect(),
            other => Err(self.refuse(
                path,
                format!(
                    "expected text or a list of text, found {}",
                    other.describe()
                ),
            )),
        }
    }

    fn paths(&self, value: Option<&Value>, path: &str) -> Result<Vec<PathBuf>, String> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        Ok(self
            .texts(value, path)?
            .into_iter()
            .map(|text| {
                if meridian_module_reference(&text) || Path::new(&text).is_absolute() {
                    PathBuf::from(text)
                } else {
                    self.base.join(text)
                }
            })
            .collect())
    }

    fn data(&self, value: Option<&Value>, path: &str) -> Result<Data, String> {
        match value {
            None | Some(Value::Null) => Ok(Data::new()),
            Some(value) => self.map(value, path).cloned(),
        }
    }

    fn only(
        &self,
        map: &BTreeMap<String, Value>,
        path: &str,
        allowed: &[&str],
    ) -> Result<(), String> {
        for key in map.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(self.refuse(
                    &format!("{path}.{key}"),
                    format!("unknown field (expected one of: {})", allowed.join(", ")),
                ));
            }
        }
        Ok(())
    }
}

/// Whether a path is written `@module:path`, which the command resolves.
fn meridian_module_reference(text: &str) -> bool {
    text.starts_with('@') && text.contains(':')
}

fn read(root: &Value, at: &str, base: &Path, path: &Path) -> Result<TestFile, String> {
    let r = Reader { at, base };
    let top = r.map(root, "")?;
    r.only(top, "", &["apiVersion", "kind", "metadata", "spec"])?;
    let api = top
        .get("apiVersion")
        .map(|v| r.text(v, "apiVersion"))
        .transpose()?;
    if api.as_deref() != Some(meridian_ir::API_VERSION_V1) {
        return Err(r.refuse(
            "apiVersion",
            format!("expected {:?}", meridian_ir::API_VERSION_V1),
        ));
    }
    let kind = top.get("kind").map(|v| r.text(v, "kind")).transpose()?;
    if kind.as_deref() != Some(TEST_KIND) {
        return Err(r.refuse(
            "kind",
            format!("expected {TEST_KIND:?}, which is what meridian test runs"),
        ));
    }
    let metadata = top
        .get("metadata")
        .ok_or_else(|| r.refuse("metadata", "missing required field"))?;
    let metadata = r.map(metadata, "metadata")?;
    r.only(metadata, "metadata", &["name", "description"])?;
    let name = metadata
        .get("name")
        .ok_or_else(|| r.refuse("metadata.name", "missing required field"))
        .and_then(|v| r.text(v, "metadata.name"))?;
    if name.trim().is_empty() {
        return Err(r.refuse("metadata.name", "must not be empty"));
    }
    let spec = top
        .get("spec")
        .ok_or_else(|| r.refuse("spec", "missing required field"))?;
    let spec = r.map(spec, "spec")?;
    r.only(
        spec,
        "spec",
        &["documents", "data", "set", "select", "cases"],
    )?;
    let documents = r.paths(spec.get("documents"), "spec.documents")?;
    if documents.is_empty() {
        return Err(r.refuse("spec.documents", "must name at least one document to test"));
    }
    let data = r.paths(spec.get("data"), "spec.data")?;
    let set = r.data(spec.get("set"), "spec.set")?;
    let select = spec
        .get("select")
        .map(|v| r.text(v, "spec.select"))
        .transpose()?;
    let cases = match spec.get("cases") {
        Some(Value::List(items)) if !items.is_empty() => items
            .iter()
            .enumerate()
            .map(|(i, item)| case(&r, item, &format!("spec.cases[{i}]")))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(r.refuse("spec.cases", "must hold at least one case")),
    };
    let mut seen = std::collections::BTreeSet::new();
    for (i, c) in cases.iter().enumerate() {
        if !seen.insert(c.name.clone()) {
            return Err(r.refuse(
                &format!("spec.cases[{i}].name"),
                format!("the name {:?} is used by an earlier case", c.name),
            ));
        }
    }
    Ok(TestFile {
        path: path.to_path_buf(),
        name,
        documents,
        data,
        set,
        select,
        cases,
    })
}

fn case(r: &Reader<'_>, value: &Value, path: &str) -> Result<Case, String> {
    let map = r.map(value, path)?;
    r.only(
        map,
        path,
        &[
            "name", "run", "data", "set", "select", "expect", "assert", "phases", "timeout",
        ],
    )?;
    let name = map
        .get("name")
        .ok_or_else(|| r.refuse(&format!("{path}.name"), "missing required field"))
        .and_then(|v| r.text(v, &format!("{path}.name")))?;
    if name.trim().is_empty() {
        return Err(r.refuse(&format!("{path}.name"), "must not be empty"));
    }
    let run = match map.get("run") {
        None => Run::Compile,
        Some(v) => match r.text(v, &format!("{path}.run"))?.as_str() {
            "compile" => Run::Compile,
            "plan" => Run::Plan,
            "lifecycle" => Run::Lifecycle,
            other => {
                return Err(r.refuse(
                    &format!("{path}.run"),
                    format!("expected \"compile\", \"plan\" or \"lifecycle\", found {other:?}"),
                ))
            }
        },
    };
    let data = r.paths(map.get("data"), &format!("{path}.data"))?;
    let set = r.data(map.get("set"), &format!("{path}.set"))?;
    let select = map
        .get("select")
        .map(|v| r.text(v, &format!("{path}.select")))
        .transpose()?;
    let mut expect = BTreeMap::new();
    if let Some(value) = map.get("expect") {
        let entries = r.map(value, &format!("{path}.expect"))?;
        for (document, value) in entries {
            expect.insert(
                document.clone(),
                expectation(r, value, &format!("{path}.expect.{document}"))?,
            );
        }
    }
    let mut assert = Vec::new();
    if let Some(value) = map.get("assert") {
        let Value::List(items) = value else {
            return Err(r.refuse(
                &format!("{path}.assert"),
                format!("expected a list of expressions, found {}", value.describe()),
            ));
        };
        for (i, item) in items.iter().enumerate() {
            assert.push(assertion(r, item, &format!("{path}.assert[{i}]"))?);
        }
    }
    let lifecycle = run == Run::Lifecycle;
    for key in ["phases", "timeout"] {
        if !lifecycle && map.contains_key(key) {
            return Err(r.refuse(
                &format!("{path}.{key}"),
                "only a lifecycle case takes this (run: lifecycle)",
            ));
        }
    }
    let phases = match map.get("phases") {
        None => Phases::default(),
        Some(value) => phases(r, value, &format!("{path}.phases"))?,
    };
    let timeout = map
        .get("timeout")
        .map(|v| {
            let at = format!("{path}.timeout");
            duration(&r.text(v, &at)?).map_err(|e| r.refuse(&at, e))
        })
        .transpose()?;
    // A lifecycle case checks something without saying so: that the
    // environment provisions and is destroyed.
    if !lifecycle && expect.is_empty() && assert.is_empty() {
        return Err(r.refuse(
            path,
            "a case must expect or assert something; a case that checks nothing cannot fail",
        ));
    }
    Ok(Case {
        name,
        run,
        data,
        set,
        select,
        expect,
        assert,
        phases,
        timeout,
    })
}

fn phases(r: &Reader<'_>, value: &Value, path: &str) -> Result<Phases, String> {
    let map = r.map(value, path)?;
    for key in ["break", "diagnose"] {
        if map.contains_key(key) {
            return Err(r.refuse(
                &format!("{path}.{key}"),
                "not supported yet: it comes with the fault catalogue",
            ));
        }
    }
    r.only(map, path, &["provision", "verify", "destroy"])?;
    for key in ["provision", "destroy"] {
        if let Some(v) = map.get(key) {
            if !matches!(v, Value::Null) {
                r.only(
                    r.map(v, &format!("{path}.{key}"))?,
                    &format!("{path}.{key}"),
                    &[],
                )?;
            }
        }
    }
    let mut out = Phases::default();
    if let Some(v) = map.get("verify") {
        if matches!(v, Value::Null) {
            return Ok(out);
        }
        let at = format!("{path}.verify");
        let verify = r.map(v, &at)?;
        r.only(verify, &at, &["drift"])?;
        if let Some(v) = verify.get("drift") {
            match r.text(v, &format!("{at}.drift"))?.as_str() {
                "none" => out.no_drift = true,
                "any" => {}
                other => {
                    return Err(r.refuse(
                        &format!("{at}.drift"),
                        format!("expected \"none\" or \"any\", found {other:?}"),
                    ))
                }
            }
        }
    }
    Ok(out)
}

/// A length of time as a person writes one: `90`, `30s`, `5m`, `1h`. A
/// bare number is seconds.
fn duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let (number, unit) = text.split_at(digits);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("{text:?} is not a duration; write one as 30s, 5m or 1h"))?;
    let seconds = match unit {
        "" | "s" => number,
        "m" => number.saturating_mul(60),
        "h" => number.saturating_mul(3600),
        other => return Err(format!("{other:?} is not a unit of time; use s, m or h")),
    };
    if seconds == 0 {
        return Err("must be longer than nothing".to_owned());
    }
    Ok(Duration::from_secs(seconds))
}

const OUTCOMES: &[&str] = &[
    "compiled", "refused", "left-out", "planned", "deferred", "failed", "applied",
];

fn expectation(r: &Reader<'_>, value: &Value, path: &str) -> Result<Expect, String> {
    let map = r.map(value, path)?;
    r.only(
        map,
        path,
        &[
            "outcome",
            "rule",
            "resource",
            "message",
            "files",
            "warnings",
            "changes",
            "changeCount",
        ],
    )?;
    let mut out = Expect::default();
    if let Some(v) = map.get("outcome") {
        let word = r.text(v, &format!("{path}.outcome"))?;
        if !OUTCOMES.contains(&word.as_str()) {
            return Err(r.refuse(
                &format!("{path}.outcome"),
                format!("expected one of {}, found {word:?}", OUTCOMES.join(", ")),
            ));
        }
        out.outcome = Some(word);
    }
    for (key, slot) in [
        ("rule", &mut out.rule),
        ("resource", &mut out.resource),
        ("message", &mut out.message),
    ] {
        if let Some(v) = map.get(key) {
            *slot = Some(r.text(v, &format!("{path}.{key}"))?);
        }
    }
    if let Some(v) = map.get("files") {
        let files = r.map(v, &format!("{path}.files"))?;
        for (file, value) in files {
            let at = format!("{path}.files.{file}");
            let fields = r.map(value, &at)?;
            r.only(
                fields,
                &at,
                &["contains", "notContains", "equals", "matches"],
            )?;
            let mut expect = FileExpect::default();
            if let Some(v) = fields.get("contains") {
                expect.contains = r.texts(v, &format!("{at}.contains"))?;
            }
            if let Some(v) = fields.get("notContains") {
                expect.not_contains = r.texts(v, &format!("{at}.notContains"))?;
            }
            if let Some(v) = fields.get("equals") {
                expect.equals = Some(r.text(v, &format!("{at}.equals"))?);
            }
            if let Some(v) = fields.get("matches") {
                expect.matches = r.texts(v, &format!("{at}.matches"))?;
                for pattern in &expect.matches {
                    // The same patterns as regexMatch, checked now and not
                    // at the first run.
                    Expr::parse(&format!("regexMatch('', {})", quote(pattern)))
                        .and_then(|e| e.eval(&meridian_expr::Scope::new()))
                        .map_err(|e| r.refuse(&format!("{at}.matches"), e))?;
                }
            }
            out.files.insert(file.clone(), expect);
        }
    }
    if let Some(v) = map.get("warnings") {
        out.warnings = r.texts(v, &format!("{path}.warnings"))?;
    }
    if let Some(v) = map.get("changes") {
        let Value::List(items) = v else {
            return Err(r.refuse(
                &format!("{path}.changes"),
                format!("expected a list, found {}", v.describe()),
            ));
        };
        for (i, item) in items.iter().enumerate() {
            let at = format!("{path}.changes[{i}]");
            let fields = r.map(item, &at)?;
            r.only(fields, &at, &["action", "resource"])?;
            let mut change = ChangeExpect::default();
            if let Some(v) = fields.get("action") {
                let word = r.text(v, &format!("{at}.action"))?;
                if !["create", "update", "delete", "replace", "adopt"].contains(&word.as_str()) {
                    return Err(r.refuse(
                        &format!("{at}.action"),
                        format!(
                            "expected create, update, delete, replace or adopt, found {word:?}"
                        ),
                    ));
                }
                change.action = Some(word);
            }
            if let Some(v) = fields.get("resource") {
                change.resource = Some(r.text(v, &format!("{at}.resource"))?);
            }
            if change.action.is_none() && change.resource.is_none() {
                return Err(r.refuse(&at, "a change must name an action, a resource or both"));
            }
            out.changes.push(change);
        }
    }
    if let Some(v) = map.get("changeCount") {
        out.change_count = Some(match v {
            Value::Int(n) if *n >= 0 => *n as usize,
            Value::Uint(n) => *n as usize,
            other => {
                return Err(r.refuse(
                    &format!("{path}.changeCount"),
                    format!("expected a whole number, found {}", other.describe()),
                ))
            }
        });
    }
    Ok(out)
}

/// An expression as a single-quoted literal of the language.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn assertion(r: &Reader<'_>, value: &Value, path: &str) -> Result<Assertion, String> {
    let (text, message) = match value {
        Value::String(text) => (text.clone(), None),
        Value::Map(map) => {
            r.only(map, path, &["expr", "message"])?;
            let text = map
                .get("expr")
                .ok_or_else(|| r.refuse(&format!("{path}.expr"), "missing required field"))
                .and_then(|v| r.text(v, &format!("{path}.expr")))?;
            let message = map
                .get("message")
                .map(|v| r.text(v, &format!("{path}.message")))
                .transpose()?;
            (text, message)
        }
        other => {
            return Err(r.refuse(
                path,
                format!(
                    "expected an expression, or a mapping of expr and message, found {}",
                    other.describe()
                ),
            ))
        }
    };
    let expr = Expr::parse(&text).map_err(|e| r.refuse(path, e))?;
    expr.check(&names()).map_err(|e| r.refuse(path, e))?;
    Ok(Assertion {
        text,
        expr,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_is_written_in_seconds_minutes_or_hours() {
        assert_eq!(duration("90"), Ok(Duration::from_secs(90)));
        assert_eq!(duration("20m"), Ok(Duration::from_secs(1200)));
        assert_eq!(duration("1h"), Ok(Duration::from_secs(3600)));
        assert!(duration("0s").is_err());
        assert!(duration("5d").is_err());
        assert!(duration("soon").is_err());
    }
}
