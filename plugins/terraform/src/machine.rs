//! Terraform's and OpenTofu's `-json` output: one JSON object a line. A
//! person is shown each line's `@message`; an error diagnostic becomes a
//! [`Failure`] on the resource its `address` names.

use meridian_sdk::decode_value;
use meridian_sdk::failure::{Cause, Failure};
use meridian_sdk::tool::{Finished, Tool};
use meridian_sdk::{Log, RunError, Value};

/// Which command the output came from: a plan's error is about the
/// configuration unless it names a resource, an apply's about the resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Plan,
    Apply,
}

/// What a person is shown of one line: its `@message`, and for a
/// diagnostic its detail too, on one line.
pub fn shown(line: &str) -> Option<String> {
    let value = decode_value(line.trim().as_bytes()).ok()?;
    let map = value.as_map()?;
    let mut text = map.get("@message")?.as_str()?.to_owned();
    let detail = map
        .get("diagnostic")
        .and_then(Value::as_map)
        .and_then(|d| d.get("detail"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !detail.is_empty() {
        text.push_str(": ");
        text.push_str(detail);
    }
    Some(text.replace('\n', " "))
}

/// Runs `tool`, which was given `-json`, and on failure adds a [`Failure`]
/// for each error diagnostic it printed.
pub fn run(tool: Tool, what: &str, phase: Phase, log: &dyn Log) -> Result<Finished, RunError> {
    let finished = tool.show_stdout_as(shown).run(log)?;
    if finished.success() {
        return Ok(finished);
    }
    let found = failures(&finished.stdout, phase);
    let mut error = finished.into_error(what);
    error.failures = found;
    Err(error)
}

/// Each error diagnostic in `-json` output, as a [`Failure`].
pub fn failures(stdout: &str, phase: Phase) -> Vec<Failure> {
    let mut found = Vec::new();
    for line in stdout.lines() {
        let Ok(value) = decode_value(line.trim().as_bytes()) else {
            continue;
        };
        let Some(map) = value.as_map() else {
            continue;
        };
        if map.get("type").and_then(Value::as_str) != Some("diagnostic") {
            continue;
        }
        let Some(diagnostic) = map.get("diagnostic").and_then(Value::as_map) else {
            continue;
        };
        let text = |key: &str| diagnostic.get(key).and_then(Value::as_str).unwrap_or("");
        if text("severity") != "error" {
            continue;
        }
        let address = text("address");
        let cause = cause(text("summary"), text("detail"), !address.is_empty(), phase);
        found.push(
            Failure::new(cause, resource_id(address), text("summary")).detail(text("detail")),
        );
    }
    found
}

/// A resource the document declares is named by its id; one inside a
/// module, or one of several, keeps Terraform's address.
fn resource_id(address: &str) -> String {
    if address.starts_with("module.") || address.starts_with("data.") || address.contains('[') {
        return address.to_owned();
    }
    match address.split_once('.') {
        Some((_, name)) if !name.contains('.') => name.to_owned(),
        _ => address.to_owned(),
    }
}

/// The cause word for a diagnostic, from what its text says. The providers
/// word their errors differently, so this looks for what they share.
fn cause(summary: &str, detail: &str, has_address: bool, phase: Phase) -> Cause {
    let text = format!("{summary} {detail}").to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if any(&[
        "no such host",
        "connection refused",
        "dial tcp",
        "i/o timeout",
        "network is unreachable",
    ]) {
        Cause::Unreachable
    } else if any(&[
        "no valid credential",
        "invalidclienttokenid",
        "authfailure",
        "authorizationfailed",
        "accessdenied",
        "access denied",
        "unauthorized",
        "forbidden",
        "could not find default credentials",
        "invalid_grant",
        "expiredtoken",
    ]) {
        Cause::AuthFailed
    } else if any(&[
        "failed to query available provider packages",
        "failed to install provider",
        "missing required provider",
        "module not installed",
        "required plugins are not installed",
        "inconsistent dependency lock file",
    ]) {
        Cause::DependencyMissing
    } else if any(&[
        "timeout while waiting",
        "context deadline exceeded",
        "timed out",
    ]) {
        Cause::TimedOut
    } else if has_address {
        Cause::ResourceFailed
    } else if phase == Phase::Plan {
        Cause::InvalidConfig
    } else {
        Cause::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines as `terraform apply -json` prints them, one resource failing.
    const APPLY: &str = r#"{"@level":"info","@message":"Terraform 1.9.5","@module":"terraform.ui","type":"version","terraform":"1.9.5","ui":"1.2"}
{"@level":"info","@message":"aws_s3_bucket.logs: Creating...","@module":"terraform.ui","hook":{"resource":{"addr":"aws_s3_bucket.logs","module":"","resource":"aws_s3_bucket.logs","implied_provider":"aws","resource_type":"aws_s3_bucket","resource_name":"logs","resource_key":null},"action":"create"},"type":"apply_start"}
{"@level":"error","@message":"aws_s3_bucket.logs: Creation errored after 1s","@module":"terraform.ui","hook":{"resource":{"addr":"aws_s3_bucket.logs","module":"","resource":"aws_s3_bucket.logs","implied_provider":"aws","resource_type":"aws_s3_bucket","resource_name":"logs","resource_key":null},"action":"create","elapsed_seconds":1},"type":"apply_errored"}
{"@level":"error","@message":"Error: creating S3 Bucket (shop-logs): operation error S3: CreateBucket, BucketAlreadyExists","@module":"terraform.ui","diagnostic":{"severity":"error","summary":"creating S3 Bucket (shop-logs): operation error S3: CreateBucket, BucketAlreadyExists","detail":"","address":"aws_s3_bucket.logs","range":{"filename":"main.tf.json","start":{"line":12,"column":9,"byte":200},"end":{"line":12,"column":10,"byte":201}}},"type":"diagnostic"}
"#;

    #[test]
    fn an_apply_error_names_the_resource_by_its_id() {
        let found = failures(APPLY, Phase::Apply);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].resource, "logs");
        assert_eq!(found[0].cause, Cause::ResourceFailed);
        assert!(found[0].summary.contains("BucketAlreadyExists"));
        assert_eq!(
            shown(APPLY.lines().nth(1).unwrap()).as_deref(),
            Some("aws_s3_bucket.logs: Creating...")
        );
        assert_eq!(shown("not json"), None);
        let diagnostic = r#"{"@message":"Error: Unsupported argument","diagnostic":{"severity":"error","summary":"Unsupported argument","detail":"An argument named \"x\" is not expected here."},"type":"diagnostic"}"#;
        assert_eq!(
            shown(diagnostic).as_deref(),
            Some("Error: Unsupported argument: An argument named \"x\" is not expected here.")
        );
    }

    #[test]
    fn a_plan_error_with_no_resource_is_the_configurations_and_words_pick_the_cause() {
        let unsupported = r#"{"@level":"error","@message":"Error: Unsupported argument","diagnostic":{"severity":"error","summary":"Unsupported argument","detail":"An argument named \"colour\" is not expected here."},"type":"diagnostic"}"#;
        assert_eq!(
            failures(unsupported, Phase::Plan)[0].cause,
            Cause::InvalidConfig
        );
        assert_eq!(failures(unsupported, Phase::Plan)[0].resource, "");
        assert_eq!(failures(unsupported, Phase::Apply)[0].cause, Cause::Unknown);

        let warning = r#"{"@level":"warn","@message":"Warning: x","diagnostic":{"severity":"warning","summary":"x","detail":""},"type":"diagnostic"}"#;
        assert!(failures(warning, Phase::Plan).is_empty());

        let creds = r#"{"diagnostic":{"severity":"error","summary":"No valid credential sources found","detail":"","address":"aws_instance.web"},"type":"diagnostic"}"#;
        assert_eq!(failures(creds, Phase::Plan)[0].cause, Cause::AuthFailed);
        assert_eq!(failures(creds, Phase::Plan)[0].resource, "web");

        let provider = r#"{"diagnostic":{"severity":"error","summary":"Failed to query available provider packages","detail":""},"type":"diagnostic"}"#;
        assert_eq!(
            failures(provider, Phase::Plan)[0].cause,
            Cause::DependencyMissing
        );

        let module = r#"{"diagnostic":{"severity":"error","summary":"timeout while waiting for state to become 'running'","detail":"","address":"module.net.aws_vpc.main"},"type":"diagnostic"}"#;
        let found = failures(module, Phase::Apply);
        assert_eq!(found[0].cause, Cause::TimedOut);
        assert_eq!(found[0].resource, "module.net.aws_vpc.main");
    }
}
