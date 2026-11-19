//! One resource as its tool reads it now, beside what the document
//! declares: what [`Mode::Inspect`] asks for and what it hands back.
//!
//! An inspection compiles the one step that declares the resource, as a
//! drift check does, and asks its plugin to read the resource. It takes no
//! lock, keeps no record and changes nothing.

use super::*;

/// What [`Mode::Inspect`] reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// The step that declares the resource: [`step_of`] finds it.
    pub step: StageId,
    /// The resource's id, as the document declares it.
    pub resource: String,
    /// The machine to read it on; empty for the step's own.
    pub endpoint: String,
}

/// Why a declared property was not compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotCompared {
    /// It holds a secret, which is never shown or compared here.
    Sensitive,
    /// The tool did not report it.
    NotReported,
}

impl NotCompared {
    /// The word the JSON output uses.
    pub fn as_str(self) -> &'static str {
        match self {
            NotCompared::Sensitive => "sensitive",
            NotCompared::NotReported => "not-reported",
        }
    }
}

/// One resource, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inspected {
    pub resource: String,
    pub r#type: String,
    /// The machine it was read on; empty for an Infrastructure step.
    pub endpoint: String,
    /// False when the tool has no such resource.
    pub found: bool,
    /// `state` and the params, as the document compiled them. A secret is
    /// written as [`meridian_sdk::SENSITIVE`].
    pub desired: Data,
    /// What the tool read, with every known secret value masked.
    pub actual: Data,
    /// The declared properties whose value the tool reads differently.
    pub differs: Vec<String>,
    /// The declared properties that were not compared, and why.
    pub not_compared: Vec<(String, NotCompared)>,
    /// What the tool says; `None` when it cannot say.
    pub in_desired_state: Option<bool>,
    /// The native command the plugin ran.
    pub command: String,
}

/// The step of `bundle` that declares `resource`: `step` when given, which
/// must declare it, otherwise the one document that does.
pub fn step_of(bundle: &Bundle, resource: &str, step: Option<&str>) -> Result<StageId, String> {
    let declaring: Vec<&Document> = bundle
        .documents
        .iter()
        .filter(|d| step.is_none_or(|s| d.name() == s))
        .filter(|d| d.resources().iter().any(|r| r.id == resource))
        .collect();
    let id = |d: &Document| StageId {
        kind: match d {
            Document::Infrastructure(_) => StageKind::Infrastructure,
            Document::ResourceSet(_) => StageKind::ResourceSet,
        },
        name: d.name().to_owned(),
    };
    match (declaring.as_slice(), step) {
        ([one], _) => Ok(id(one)),
        ([], Some(step)) if !bundle.documents.iter().any(|d| d.name() == step) => {
            Err(format!("no document is named {step:?}"))
        }
        ([], Some(step)) => Err(format!("{step} declares no resource {resource:?}")),
        ([], None) => Err(format!("no document declares a resource {resource:?}")),
        (many, _) => Err(format!(
            "{} declare a resource {resource:?}; name one with --step",
            many.iter().map(|d| d.name()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Puts what the document declares beside what the tool read.
pub(crate) fn compare(
    declared: &meridian_sdk::Resource,
    endpoint: &str,
    read: meridian_sdk::Inspection,
    masked: impl Fn(&str) -> String,
) -> Inspected {
    let mut desired = Data::new();
    if !declared.state.is_empty() {
        desired.insert("state".to_owned(), Value::from(declared.state.as_str()));
    }
    desired.extend(declared.params.clone());

    let mut actual = masking(read.actual, &masked);
    // A tool that says only whether the thing exists still answers a
    // declared present or absent.
    if !actual.contains_key("state") && matches!(declared.state.as_str(), "present" | "absent") {
        let state = if read.found { "present" } else { "absent" };
        actual.insert("state".to_owned(), Value::from(state));
    }

    let mut differs = Vec::new();
    let mut not_compared = Vec::new();
    for (name, want) in &desired {
        if holds_secret(want) {
            not_compared.push((name.clone(), NotCompared::Sensitive));
            continue;
        }
        match actual.get(name) {
            None => not_compared.push((name.clone(), NotCompared::NotReported)),
            Some(have) if !same(want, have) => differs.push(name.clone()),
            Some(_) => {}
        }
    }
    // The values only stand for the secrets from here on.
    let desired = desired
        .into_iter()
        .map(|(k, v)| {
            if holds_secret(&v) {
                (k, Value::from(meridian_sdk::SENSITIVE))
            } else {
                (k, v)
            }
        })
        .collect();
    Inspected {
        resource: declared.id.clone(),
        r#type: declared.r#type.clone(),
        endpoint: endpoint.to_owned(),
        found: read.found,
        desired,
        actual,
        differs,
        not_compared,
        in_desired_state: read.in_desired_state,
        command: masked(&read.command),
    }
}

fn holds_secret(value: &Value) -> bool {
    match value {
        Value::Secret(_) => true,
        Value::List(items) => items.iter().any(holds_secret),
        Value::Map(map) => map.values().any(holds_secret),
        _ => false,
    }
}

/// Every string of `data`, masked.
fn masking(data: Data, masked: &impl Fn(&str) -> String) -> Data {
    fn walk(value: Value, masked: &impl Fn(&str) -> String) -> Value {
        match value {
            Value::String(s) => Value::String(masked(&s)),
            Value::List(items) => Value::List(items.into_iter().map(|v| walk(v, masked)).collect()),
            Value::Map(map) => {
                Value::Map(map.into_iter().map(|(k, v)| (k, walk(v, masked))).collect())
            }
            other => other,
        }
    }
    data.into_iter()
        .map(|(k, v)| (k, walk(v, masked)))
        .collect()
}

/// Whether a declared value and a read one are the same. A tool reads
/// most things back as text, so `80` and `"80"` are, and so are `true`
/// and `"true"`.
fn same(want: &Value, have: &Value) -> bool {
    if want == have {
        return true;
    }
    match (want, have) {
        (Value::String(a), b) | (b, Value::String(a)) if is_scalar(b) => *a == scalar_text(b),
        (Value::Int(a), Value::Uint(b)) | (Value::Uint(b), Value::Int(a)) => {
            u64::try_from(*a).is_ok_and(|a| a == *b)
        }
        (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => (*a as f64) == *b,
        (Value::List(a), Value::List(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|other| same(v, other)))
        }
        _ => false,
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Bool(_) | Value::Int(_) | Value::Uint(_) | Value::Float(_)
    )
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Uint(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{Inspection, SecretRef};

    fn declared(params: &[(&str, Value)]) -> meridian_sdk::Resource {
        meridian_sdk::Resource {
            id: "web".to_owned(),
            r#type: "service".to_owned(),
            state: "present".to_owned(),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            ..Default::default()
        }
    }

    fn read(found: bool, actual: &[(&str, Value)]) -> Inspection {
        Inspection {
            found,
            actual: actual
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            in_desired_state: None,
            command: "tool read web".to_owned(),
        }
    }

    #[test]
    fn text_read_back_is_the_same_as_a_number() {
        let got = compare(
            &declared(&[("port", Value::Int(80)), ("enabled", Value::Bool(true))]),
            "web1",
            read(
                true,
                &[
                    ("port", Value::from("80")),
                    ("enabled", Value::from("false")),
                ],
            ),
            str::to_owned,
        );
        assert_eq!(got.differs, vec!["enabled".to_owned()]);
        assert_eq!(got.actual.get("state"), Some(&Value::from("present")));
    }

    #[test]
    fn a_missing_resource_differs_in_state() {
        let got = compare(&declared(&[]), "web1", read(false, &[]), str::to_owned);
        assert_eq!(got.differs, vec!["state".to_owned()]);
        assert!(!got.found);
    }

    #[test]
    fn a_secret_is_never_compared_or_shown() {
        let got = compare(
            &declared(&[("password", Value::Secret(SecretRef::new(0)))]),
            "web1",
            read(true, &[("password", Value::from("hunter22"))]),
            |text: &str| text.replace("hunter22", MASK),
        );
        assert_eq!(
            got.not_compared,
            vec![("password".to_owned(), NotCompared::Sensitive)]
        );
        assert_eq!(
            got.desired.get("password"),
            Some(&Value::from(meridian_sdk::SENSITIVE))
        );
        assert_eq!(got.actual.get("password"), Some(&Value::from(MASK)));
    }

    #[test]
    fn what_the_tool_does_not_report_is_said_so() {
        let got = compare(
            &declared(&[("owner", Value::from("root"))]),
            "web1",
            read(true, &[]),
            str::to_owned,
        );
        assert_eq!(
            got.not_compared,
            vec![("owner".to_owned(), NotCompared::NotReported)]
        );
        assert!(got.differs.is_empty());
    }
}
