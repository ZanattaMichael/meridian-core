//! `lab/faults/`: how to break a provisioned environment and what Meridian
//! must then report. A `Fault` document names the category it applies to, how
//! the fault is injected, and what the run, the cause word, the diagnose
//! command and (once #223 and #225 land) the probe and the known failure must
//! say afterwards. See issue 237.

use std::path::{Path, PathBuf};

use meridian_value::Value;

use crate::document::Reader;

/// The `kind` of a fault document.
pub const FAULT_KIND: &str = "Fault";

/// One `Fault` document.
#[derive(Clone, Debug, PartialEq)]
pub struct FaultFile {
    pub path: PathBuf,
    /// `metadata.name`.
    pub name: String,
    /// What the fault applies to.
    pub applies: Applies,
    /// How the environment is broken.
    pub inject: Inject,
    /// What Meridian must report once it is broken.
    pub expect: FaultExpect,
}

/// `spec.applies`.
#[derive(Clone, Debug, PartialEq)]
pub struct Applies {
    /// The document category the fault applies to, as the category tags
    /// name it, for example `configuration`.
    pub category: String,
}

/// `spec.inject`: exactly one way to break the environment.
#[derive(Clone, Debug, PartialEq)]
pub enum Inject {
    /// A command run on the endpoint, after provisioning.
    Command(Vec<String>),
    /// A Meridian document applied outside the provisioned run's state, so
    /// the tool's own record no longer matches the world.
    Document(PathBuf),
}

/// `spec.expect`: what a run over the broken environment must come to.
#[derive(Clone, Debug, PartialEq)]
pub struct FaultExpect {
    /// The run must fail (the only value v1 checks).
    pub run: ExpectRun,
    /// The failure's cause word, from the registry (#213).
    pub cause: String,
    /// The resource the failure names.
    pub resource: String,
    /// The endpoint the failure names, where the fault names one.
    pub endpoint: Option<String>,
    /// `meridian diagnose` must name this resource first (#215).
    pub diagnose_first: Option<String>,
    /// The probe that must find the fault (#223); parsed, not yet asserted.
    pub probe: Option<String>,
    /// The known failure that must match (#225); parsed, not yet asserted.
    pub known_failure: Option<String>,
}

/// What the run must do. Only failure is an expectation today; a fault whose
/// environment still applies cleanly is itself the failure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExpectRun {
    Failed,
}

/// Reads the one `Fault` document in the file.
pub fn load_fault(path: &Path) -> Result<FaultFile, String> {
    let shown = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|e| format!("{shown}: cannot read file: {e}"))?;
    let base = path.parent().unwrap_or(Path::new("."));
    parse_fault(&shown, &bytes, base)
}

fn parse_fault(shown: &str, bytes: &[u8], base: &Path) -> Result<FaultFile, String> {
    let roots = meridian_ir::parse_stream(shown, bytes).map_err(|e| e.to_string())?;
    let [root] = roots.as_slice() else {
        return Err(format!(
            "{shown}: a fault file holds exactly one Fault document"
        ));
    };
    let r = Reader { at: shown, base };
    let top = r.map(root, "")?;
    r.only(top, "", &["apiVersion", "kind", "metadata", "spec"])?;
    if top.get("apiVersion").and_then(Value::as_str) != Some(meridian_ir::API_VERSION_V1) {
        return Err(r.refuse(
            "apiVersion",
            format!("expected {:?}", meridian_ir::API_VERSION_V1),
        ));
    }
    if top.get("kind").and_then(Value::as_str) != Some(FAULT_KIND) {
        return Err(r.refuse(
            "kind",
            format!("expected {FAULT_KIND:?}, which is what a fault catalogue holds"),
        ));
    }
    let metadata = r.map(
        top.get("metadata")
            .ok_or_else(|| r.refuse("metadata", "missing required field"))?,
        "metadata",
    )?;
    r.only(metadata, "metadata", &["name", "description"])?;
    let name = r.text(
        metadata
            .get("name")
            .ok_or_else(|| r.refuse("metadata.name", "missing required field"))?,
        "metadata.name",
    )?;
    if crate::run::slug(&name) != name {
        return Err(r.refuse(
            "metadata.name",
            "lowercase letters, digits and -, as it names a directory",
        ));
    }
    let spec = r.map(
        top.get("spec")
            .ok_or_else(|| r.refuse("spec", "missing required field"))?,
        "spec",
    )?;
    r.only(spec, "spec", &["applies", "inject", "expect"])?;

    let applies_value = r.map(
        spec.get("applies")
            .ok_or_else(|| r.refuse("spec.applies", "missing required field"))?,
        "spec.applies",
    )?;
    r.only(applies_value, "spec.applies", &["category"])?;
    let category = r.text(
        applies_value
            .get("category")
            .ok_or_else(|| r.refuse("spec.applies.category", "missing required field"))?,
        "spec.applies.category",
    )?;

    let inject = parse_inject(&r, spec.get("inject"))?;
    let expect = parse_expect(&r, spec.get("expect"))?;
    Ok(FaultFile {
        path: base.join(&name),
        name,
        applies: Applies { category },
        inject,
        expect,
    })
}

fn parse_inject(r: &Reader<'_>, value: Option<&Value>) -> Result<Inject, String> {
    let value = r.map(
        value.ok_or_else(|| r.refuse("spec.inject", "missing required field"))?,
        "spec.inject",
    )?;
    r.only(value, "spec.inject", &["command", "document"])?;
    let command = value.get("command");
    let document = value.get("document");
    match (command, document) {
        (Some(command), None) => {
            let Value::List(items) = command else {
                return Err(r.refuse("spec.inject.command", "expected a list of words"));
            };
            if items.is_empty() {
                return Err(r.refuse("spec.inject.command", "must hold at least one word"));
            }
            let words = items
                .iter()
                .enumerate()
                .map(|(i, item)| r.text(item, &format!("spec.inject.command[{i}]")))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Inject::Command(words))
        }
        (None, Some(document)) => {
            let path = r.text(document, "spec.inject.document")?;
            Ok(Inject::Document(if Path::new(&path).is_absolute() {
                PathBuf::from(path)
            } else {
                r.base.join(path)
            }))
        }
        (Some(_), Some(_)) => Err(r.refuse(
            "spec.inject",
            "names both command and document; a fault injects one way",
        )),
        (None, None) => Err(r.refuse(
            "spec.inject",
            "names neither command nor document; a fault must say how it is injected",
        )),
    }
}

fn parse_expect(r: &Reader<'_>, value: Option<&Value>) -> Result<FaultExpect, String> {
    let value = r.map(
        value.ok_or_else(|| r.refuse("spec.expect", "missing required field"))?,
        "spec.expect",
    )?;
    r.only(
        value,
        "spec.expect",
        &[
            "run",
            "cause",
            "resource",
            "endpoint",
            "diagnose",
            "probe",
            "knownFailure",
        ],
    )?;
    match value.get("run").and_then(Value::as_str) {
        Some("failed") => {}
        Some(other) => {
            return Err(r.refuse(
                "spec.expect.run",
                format!("expected \"failed\", found {other:?}; a fault expects a failed run"),
            ))
        }
        None => return Err(r.refuse("spec.expect.run", "missing required field")),
    }
    let text = |r: &Reader<'_>, field: &str| -> Result<String, String> {
        r.text(
            value.get(field).ok_or_else(|| {
                r.refuse(&format!("spec.expect.{field}"), "missing required field")
            })?,
            &format!("spec.expect.{field}"),
        )
    };
    let optional = |field: &str| -> Option<String> {
        match value.get(field) {
            Some(Value::Null) | None => None,
            Some(v) => v.as_str().map(str::to_owned),
        }
    };
    let cause = text(r, "cause")?;
    let resource = text(r, "resource")?;
    let diagnose = match value.get("diagnose") {
        Some(Value::Null) | None => None,
        Some(diagnose) => {
            let diagnose = r.map(diagnose, "spec.expect.diagnose")?;
            r.only(diagnose, "spec.expect.diagnose", &["first"])?;
            Some(r.text(
                diagnose.get("first").ok_or_else(|| {
                    r.refuse("spec.expect.diagnose.first", "missing required field")
                })?,
                "spec.expect.diagnose.first",
            )?)
        }
    };
    Ok(FaultExpect {
        run: ExpectRun::Failed,
        cause,
        resource,
        endpoint: optional("endpoint"),
        diagnose_first: diagnose,
        probe: optional("probe"),
        known_failure: optional("knownFailure"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<FaultFile, String> {
        parse_fault("fault.yaml", yaml.as_bytes(), Path::new("lab/faults"))
    }

    const SERVICE_STOPPED: &str = "\
apiVersion: meridian/v1
kind: Fault
metadata: { name: service-stopped }
spec:
  applies: { category: configuration }
  inject:
    command: [systemctl, stop, \"${{ fault.service }}\"]
  expect:
    run: failed
    cause: unavailable
    resource: service.web
    endpoint: \"${{ endpoint.name }}\"
    diagnose: { first: service.web }
    probe: service-running
    knownFailure: null
";

    #[test]
    fn parses_the_catalogue_example() {
        let fault = parse(SERVICE_STOPPED).unwrap();
        assert_eq!(fault.name, "service-stopped");
        assert_eq!(fault.applies.category, "configuration");
        assert_eq!(
            fault.inject,
            Inject::Command(vec![
                "systemctl".into(),
                "stop".into(),
                "${{ fault.service }}".into()
            ])
        );
        assert_eq!(fault.expect.cause, "unavailable");
        assert_eq!(fault.expect.resource, "service.web");
        assert_eq!(
            fault.expect.endpoint.as_deref(),
            Some("${{ endpoint.name }}")
        );
        assert_eq!(fault.expect.diagnose_first.as_deref(), Some("service.web"));
        assert_eq!(fault.expect.probe.as_deref(), Some("service-running"));
        assert_eq!(fault.expect.known_failure, None);
        assert_eq!(fault.path, Path::new("lab/faults/service-stopped"));
    }

    #[test]
    fn resolves_a_relative_document_against_the_faults_directory() {
        let fault = parse(
            "apiVersion: meridian/v1\nkind: Fault\nmetadata: { name: drift }\nspec:\n  applies: { category: configuration }\n  inject:\n    document: ../inject/delete-user.yaml\n  expect:\n    run: failed\n    cause: conflict\n    resource: user.deploy\n",
        )
        .unwrap();
        assert_eq!(
            fault.inject,
            Inject::Document(PathBuf::from("lab/faults/../inject/delete-user.yaml"))
        );
    }

    #[test]
    fn refuses_everything_missing_or_extra() {
        for (yaml, needle) in [
            ("apiVersion: meridian/v1\nkind: Fault\nmetadata: { name: x }\nspec: {}\n", "spec.applies"),
            (
                "apiVersion: meridian/v1\nkind: Fault\nmetadata: { name: x }\nspec:\n  applies: { category: configuration }\n  inject: { command: [a] }\n  expect: { run: failed, cause: c, resource: r }\n  other: 1\n",
                "spec.other",
            ),
            (
                "apiVersion: meridian/v1\nkind: Fault\nmetadata: { name: x }\nspec:\n  applies: { category: configuration }\n  inject: {}\n  expect: { run: failed, cause: c, resource: r }\n",
                "spec.inject",
            ),
            (
                "apiVersion: meridian/v1\nkind: Fault\nmetadata: { name: x }\nspec:\n  applies: { category: configuration }\n  inject: { command: [a], document: b.yaml }\n  expect: { run: failed, cause: c, resource: r }\n",
                "spec.inject",
            ),
            (
                "apiVersion: meridian/v1\nkind: Fault\nmetadata: { name: x }\nspec:\n  applies: { category: configuration }\n  inject: { command: [a] }\n  expect: { run: passed, cause: c, resource: r }\n",
                "spec.expect.run",
            ),
            (
                "apiVersion: meridian/v1\nkind: Fault\nmetadata: { name: x }\nspec:\n  applies: { category: configuration }\n  inject: { command: [] }\n  expect: { run: failed, cause: c, resource: r }\n",
                "spec.inject.command",
            ),
        ] {
            let err = parse(yaml).unwrap_err();
            assert!(err.contains(needle), "{err} (wanted {needle})");
        }
    }

    #[test]
    fn the_name_slugs_like_a_lab() {
        let err = parse(
            "apiVersion: meridian/v1\nkind: Fault\nmetadata: { name: 'Bad Name' }\nspec:\n  applies: { category: configuration }\n  inject: { command: [a] }\n  expect: { run: failed, cause: c, resource: r }\n",
        )
        .unwrap_err();
        assert!(err.contains("metadata.name"), "{err}");
    }
}
