//! What kubectl and Helm print when the API server refuses an object or an
//! object never becomes ready, read into failures. Shared with the helm
//! target, which maps the objects to its own ids.

use meridian_sdk::failure::{Cause, Failure};

/// A failure on one object, named as the tool named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectFailure {
    /// The kind as the tool wrote it: `Deployment.apps`, `deployments.apps`,
    /// `ConfigMap`. Empty when the tool named no object.
    pub kind: String,
    /// The object's name; empty when the tool named only its kind.
    pub name: String,
    /// The failure, its `resource` left for the caller to fill in.
    pub failure: Failure,
}

/// Reads every error kubectl or Helm printed. One error starts at a line
/// such as `Error from server (Invalid): ...`, `error: ...`, `Error: ...`,
/// a `* ` item of a list of errors, or Helm's `resource not ready, ...`; the
/// lines up to the next one belong to it.
pub fn read_errors(text: &str) -> Vec<ObjectFailure> {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if starts_an_error(trimmed) {
            blocks.push(vec![trimmed]);
        } else if let Some(block) = blocks.last_mut() {
            block.push(trimmed);
        }
    }
    let mut failures: Vec<ObjectFailure> = Vec::new();
    for block in &blocks {
        let Some(found) = read_block(block) else {
            continue;
        };
        // A bare "context deadline exceeded" after Helm named what was not
        // ready says nothing more.
        if found.kind.is_empty()
            && found.failure.cause == Cause::TimedOut
            && failures.iter().any(|f| f.failure.cause == Cause::TimedOut)
        {
            continue;
        }
        if !failures
            .iter()
            .any(|f| f.kind == found.kind && f.name == found.name && !f.kind.is_empty())
        {
            failures.push(found);
        }
    }
    failures
}

/// Whether `kind` as a tool wrote it (`Deployment.apps`, `deployments`) is
/// the kind a manifest declares (`Deployment`).
pub fn same_kind(written: &str, kind: &str) -> bool {
    let written = written
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = kind.to_ascii_lowercase();
    !kind.is_empty()
        && (written == kind
            || written == format!("{kind}s")
            || written == format!("{kind}es")
            || kind
                .strip_suffix('y')
                .is_some_and(|stem| written == format!("{stem}ies")))
}

fn starts_an_error(line: &str) -> bool {
    line.starts_with("* ")
        || line.starts_with("Error from server")
        || line.starts_with("error:")
        || line.starts_with("Error:")
        || line.starts_with("Unable to connect to the server")
        || line.starts_with("The connection to the server")
        || line.starts_with("resource not ready")
        || line == "context deadline exceeded"
}

fn read_block(block: &[&str]) -> Option<ObjectFailure> {
    let first = block[0].strip_prefix("* ").unwrap_or(block[0]);
    let whole = block.join("\n");
    if let Some(found) = block.iter().find_map(|line| not_ready(line)) {
        return Some(found);
    }
    let reason = first
        .strip_prefix("Error from server (")
        .and_then(|rest| rest.split_once(')'))
        .map(|(reason, _)| reason.to_owned())
        .unwrap_or_default();
    let cause = cause_of(&reason, &whole)?;
    // The line that names the object is the last one that says anything
    // about it: a patch's error ends with `for: ... Deployment.apps "web" is
    // invalid`.
    let summary_line = block
        .iter()
        .rev()
        .find(|line| object_in(line).is_some())
        .copied()
        .unwrap_or(first);
    let (mut kind, mut name) = block
        .iter()
        .rev()
        .find_map(|line| object_in(line))
        .unwrap_or_default();
    if cause == Cause::DependencyMissing && reason == "NotFound" {
        // What is named is the thing that is missing, not the object
        // that failed.
        kind.clear();
        name.clear();
    }
    let mut failure = Failure::new(cause, "", plain_summary(summary_line)).detail(whole);
    if !reason.is_empty() {
        failure = failure.code(reason);
    }
    Some(ObjectFailure {
        kind,
        name,
        failure,
    })
}

/// Helm's wait: `resource not ready, name: web, kind: Deployment, status:
/// InProgress`. An object still in progress when the wait ran out timed
/// out; one whose status is `Failed` failed.
fn not_ready(line: &str) -> Option<ObjectFailure> {
    let rest = &line[line.find("resource not ready, name: ")?..];
    let rest = rest.strip_prefix("resource not ready, name: ")?;
    let (name, rest) = rest.split_once(", kind: ")?;
    let (kind, status) = rest.split_once(", status: ")?;
    let status = status.trim();
    let cause = if status == "Failed" {
        Cause::ResourceFailed
    } else {
        Cause::TimedOut
    };
    Some(ObjectFailure {
        kind: kind.to_owned(),
        name: name.to_owned(),
        failure: Failure::new(cause, "", format!("{kind} {name} not ready: {status}")).code(status),
    })
}

/// The cause the API server's reason gives, or else what the words say.
/// `None` for a line that is not about a failure Meridian can name.
fn cause_of(reason: &str, text: &str) -> Option<Cause> {
    let lower = text.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let webhook = has(&["admission webhook"]) && has(&["denied the request"]);
    Some(match reason {
        "Forbidden" if webhook => Cause::InvalidConfig,
        "Forbidden" | "Unauthorized" => Cause::AuthFailed,
        "Invalid" | "BadRequest" | "UnsupportedMediaType" => Cause::InvalidConfig,
        "NotFound" => Cause::DependencyMissing,
        "AlreadyExists" | "Conflict" => Cause::ResourceFailed,
        "Timeout" | "ServerTimeout" => Cause::TimedOut,
        "InternalError" => Cause::from_message(text).unwrap_or(Cause::ResourceFailed),
        _ if has(&["no matches for kind", "ensure crds are installed"]) => Cause::DependencyMissing,
        _ if webhook => Cause::InvalidConfig,
        _ if has(&[" is invalid", "error validating", "cannot be handled as a"]) => {
            Cause::InvalidConfig
        }
        _ if has(&[" is forbidden", "you must be logged in"]) => Cause::AuthFailed,
        _ if has(&[
            "unable to connect to the server",
            "the connection to the server",
        ]) =>
        {
            Cause::Unreachable
        }
        _ if has(&["already exists"]) => Cause::ResourceFailed,
        _ => Cause::from_message(text)?,
    })
}

/// The object a line names, as `(kind, name)`: `Deployment.apps "web"`,
/// `no matches for kind "Widget"` with `name: "w"`, `ValidationError(ConfigMap.data)`
/// or `cannot be handled as a ConfigMap` (kind only).
fn object_in(line: &str) -> Option<(String, String)> {
    if let Some(rest) = after(line, "no matches for kind \"") {
        let kind = rest.split('"').next().unwrap_or_default();
        let name = after(line, "name: \"")
            .and_then(|r| r.split('"').next())
            .unwrap_or_default();
        return Some((kind.to_owned(), name.to_owned()));
    }
    // Every `"name"` with the word before it a kind.
    let mut search = 0;
    while let Some(open) = line[search..].find('"').map(|i| i + search) {
        let Some(close) = line[open + 1..].find('"').map(|i| i + open + 1) else {
            break;
        };
        let token = line[..open]
            .trim_end()
            .rsplit(' ')
            .next()
            .unwrap_or_default();
        if is_kind(token) {
            return Some((token.to_owned(), line[open + 1..close].to_owned()));
        }
        search = close + 1;
    }
    if let Some(rest) = after(line, "ValidationError(") {
        let kind = rest.split(['.', ')']).next().unwrap_or_default();
        return Some((kind.to_owned(), String::new()));
    }
    if let Some(rest) = after(line, "cannot be handled as a ") {
        let kind = rest.split([':', ' ']).next().unwrap_or_default();
        return Some((kind.to_owned(), String::new()));
    }
    None
}

fn after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    line.find(marker).map(|i| &line[i + marker.len()..])
}

/// Whether the word before a quoted name is a kind (`Deployment.apps`,
/// `configmaps`) rather than one of the words kubectl puts before a file
/// name, a user or a group.
fn is_kind(token: &str) -> bool {
    const NOT_KINDS: &[&str] = &[
        "creating",
        "patching",
        "applying",
        "deleting",
        "updating",
        "retrieving",
        "getting",
        "validating",
        "user",
        "resource",
        "group",
        "namespace",
        "kind",
        "version",
        "for",
        "when",
        "name",
        "from",
        "to",
        "webhook",
        "field",
        "type",
    ];
    token
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !NOT_KINDS.contains(&token.to_ascii_lowercase().as_str())
}

/// The line without the prefixes that only say an error follows.
fn plain_summary(line: &str) -> String {
    let mut line = line.strip_prefix("* ").unwrap_or(line);
    for prefix in ["Error: ", "error: ", "for: "] {
        line = line.strip_prefix(prefix).unwrap_or(line);
    }
    if let Some(rest) = line.strip_prefix("Error from server (") {
        line = rest.split_once("): ").map_or(line, |(_, rest)| rest);
    }
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Vec<(String, String, Cause, String)> {
        read_errors(text)
            .into_iter()
            .map(|f| (f.kind, f.name, f.failure.cause, f.failure.tool_code))
            .collect()
    }

    fn one(kind: &str, name: &str, cause: Cause, code: &str) -> (String, String, Cause, String) {
        (kind.into(), name.into(), cause, code.into())
    }

    #[test]
    fn an_invalid_object_is_named_with_the_api_servers_reason() {
        let stderr = "Warning: something deprecated\n\
            Error from server (Invalid): error when creating \"manifests.yaml\": Deployment.apps \"web\" is invalid: spec.replicas: Invalid value: -1\n\
            Error from server (BadRequest): error when creating \"manifests.yaml\": ConfigMap in version \"v1\" cannot be handled as a ConfigMap: json: cannot unmarshal object into Go struct field ConfigMap.data of type string\n";
        assert_eq!(
            read(stderr),
            [
                one("Deployment.apps", "web", Cause::InvalidConfig, "Invalid"),
                one("ConfigMap", "", Cause::InvalidConfig, "BadRequest"),
            ]
        );
        let failures = read_errors(stderr);
        assert_eq!(
            failures[0].failure.summary,
            "error when creating \"manifests.yaml\": Deployment.apps \"web\" is invalid: spec.replicas: Invalid value: -1"
        );
    }

    #[test]
    fn a_patch_is_named_from_its_last_line() {
        let stderr = "Error from server (Invalid): error when applying patch:\n\
            {\"spec\":{\"replicas\":-1}}\n\
            to:\n\
            Resource: \"apps/v1, Resource=deployments\", GroupVersionKind: \"apps/v1, Kind=Deployment\"\n\
            Name: \"web\", Namespace: \"default\"\n\
            for: \"manifests.yaml\": error when patching \"manifests.yaml\": Deployment.apps \"web\" is invalid: spec.replicas: Invalid value: -1\n";
        assert_eq!(
            read(stderr),
            [one(
                "Deployment.apps",
                "web",
                Cause::InvalidConfig,
                "Invalid"
            )]
        );
    }

    #[test]
    fn refusals_missing_kinds_and_a_cluster_out_of_reach_have_their_causes() {
        let stderr = "Error from server (Forbidden): error when creating \"-\": deployments.apps is forbidden: User \"ci\" cannot create resource \"deployments\" in API group \"apps\" in the namespace \"default\"\n\
            Error from server (Forbidden): error when creating \"-\": admission webhook \"validate.kyverno.svc\" denied the request: ConfigMap \"settings\" breaks policy\n\
            Error from server (NotFound): error when creating \"-\": namespaces \"shop\" not found\n\
            Error from server (AlreadyExists): error when creating \"-\": configmaps \"settings\" already exists\n\
            error: resource mapping not found for name: \"w\" namespace: \"\" from \"-\": no matches for kind \"Widget\" in version \"example.com/v1\"\n\
            ensure CRDs are installed first\n";
        assert_eq!(
            read(stderr),
            [
                one("", "", Cause::AuthFailed, "Forbidden"),
                one("ConfigMap", "settings", Cause::InvalidConfig, "Forbidden"),
                one("", "", Cause::DependencyMissing, "NotFound"),
                one(
                    "configmaps",
                    "settings",
                    Cause::ResourceFailed,
                    "AlreadyExists"
                ),
                one("Widget", "w", Cause::DependencyMissing, ""),
            ]
        );
        assert_eq!(
            read("The connection to the server localhost:8080 was refused - did you specify the right host or port?\n"),
            [one("", "", Cause::Unreachable, "")]
        );
        assert_eq!(
            read("error: You must be logged in to the server (Unauthorized)\n"),
            [one("", "", Cause::AuthFailed, "")]
        );
        assert!(read("Warning: only a warning\nnothing else\n").is_empty());
    }

    #[test]
    fn helms_wait_names_what_was_not_ready() {
        let stderr = "Error: INSTALLATION FAILED: resource not ready, name: shop-web, kind: Deployment, status: InProgress\n\
            resource not ready, name: shop-job, kind: Job, status: Failed\n\
            context deadline exceeded\n";
        assert_eq!(
            read(stderr),
            [
                one("Deployment", "shop-web", Cause::TimedOut, "InProgress"),
                one("Job", "shop-job", Cause::ResourceFailed, "Failed"),
            ]
        );
        assert_eq!(
            read("Error: UPGRADE FAILED: context deadline exceeded\n"),
            [one("", "", Cause::TimedOut, "")]
        );
    }

    #[test]
    fn helms_list_of_errors_is_read_item_by_item() {
        let stderr = "Error: INSTALLATION FAILED: 1 error occurred:\n\
            \t* Deployment.apps \"shop-web\" is invalid: spec.template.spec.containers[0].image: Required value\n";
        assert_eq!(
            read(stderr),
            [one("Deployment.apps", "shop-web", Cause::InvalidConfig, "")]
        );
    }

    #[test]
    fn a_written_kind_is_matched_to_a_declared_one() {
        assert!(same_kind("Deployment.apps", "Deployment"));
        assert!(same_kind("deployments.apps", "Deployment"));
        assert!(same_kind(
            "networkpolicies.networking.k8s.io",
            "NetworkPolicy"
        ));
        assert!(same_kind("ingresses", "Ingress"));
        assert!(!same_kind("services", "ServiceAccount"));
        assert!(!same_kind("", ""));
    }
}
