//! The analyzer for [Trivy](https://trivy.dev/)'s misconfiguration
//! scanner.
//!
//! `meridian-analyze-trivy` runs `trivy config --format json <dir>` over a
//! stage's compiled files and reports each misconfiguration whose status is
//! `FAIL` as a finding: the check id (`AVD-AWS-0088`) as the rule, Trivy's
//! title and message as the message, the file and start line it points at,
//! Trivy's own severity, and the Meridian resource id, which is the last
//! part of the resource Trivy names.
//!
//! The program is `trivy` on the `PATH`, or whatever `MERIDIAN_TRIVY`
//! names.
//!
//! Settings (`--analyze-config trivy.KEY=VALUE`):
//!
//! | Key | Meaning |
//! |---|---|
//! | `severity` | report only these severities (`--severity`), comma-separated, as Trivy spells them |
//! | `skip-check` | ignore these check ids, comma-separated |
//! | `args` | more arguments for `trivy config`, split on whitespace |

use meridian_analyze::{
    program, relative_path, resource_id, run_tool, severity, Answer, Finding, Input,
};
use meridian_value::{decode_value, Value};

/// The variable that names the `trivy` program.
pub const TRIVY_VARIABLE: &str = "MERIDIAN_TRIVY";

/// The arguments `trivy` is run with for `input`.
pub fn arguments(input: &Input) -> Vec<String> {
    let mut args = vec![
        "config".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
        "--quiet".to_owned(),
    ];
    let severities = input.setting("severity");
    if !severities.is_empty() {
        args.push("--severity".to_owned());
        args.push(severities);
    }
    args.extend(input.extra_args());
    args.push(".".to_owned());
    args
}

/// Runs Trivy over the stage and translates what it found.
pub fn analyze(input: &Input) -> Result<Answer, String> {
    let trivy = program(TRIVY_VARIABLE, "trivy");
    let output = run_tool(&trivy, &arguments(input), TRIVY_VARIABLE, &[0])?;
    translate(&output.stdout, input)
}

/// Turns Trivy's JSON report into the findings.
pub fn translate(stdout: &[u8], input: &Input) -> Result<Answer, String> {
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Answer::default());
    }
    let skipped: Vec<String> = input
        .setting("skip-check")
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let value = decode_value(stdout).map_err(|e| format!("trivy did not answer in JSON: {e}"))?;
    let results = value
        .as_map()
        .and_then(|top| top.get("Results"))
        .and_then(Value::as_list)
        .map(<[Value]>::to_vec)
        .unwrap_or_default();
    let mut answer = Answer::default();
    for result in &results {
        let Some(result) = result.as_map() else {
            continue;
        };
        let target = match result.get("Target") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let Some(found) = result.get("Misconfigurations").and_then(Value::as_list) else {
            continue;
        };
        for item in found {
            let Some(item) = item.as_map() else { continue };
            let text = |key: &str| -> String {
                match item.get(key) {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                }
            };
            if text("Status") != "FAIL" {
                continue;
            }
            let rule = {
                let id = text("ID");
                if id.is_empty() {
                    text("AVDID")
                } else {
                    id
                }
            };
            if rule.is_empty() || skipped.contains(&rule) {
                continue;
            }
            let cause = item.get("CauseMetadata").and_then(Value::as_map);
            let line = match cause.and_then(|c| c.get("StartLine")) {
                Some(Value::Uint(n)) => *n,
                Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0),
                _ => 0,
            };
            let resource = match cause.and_then(|c| c.get("Resource")) {
                Some(Value::String(s)) => resource_id(s),
                _ => String::new(),
            };
            let title = text("Title");
            let detail = text("Message");
            let message = match (title.is_empty(), detail.is_empty()) {
                (false, false) if title != detail => format!("{title}: {detail}"),
                (false, _) => title,
                (true, false) => detail,
                (true, true) => text("Description"),
            };
            answer.findings.push(Finding {
                rule,
                severity: severity(&text("Severity"), "medium"),
                message,
                file: relative_path(&target, &input.dir),
                line,
                resource,
                url: text("PrimaryURL"),
            });
        }
    }
    Ok(answer)
}
