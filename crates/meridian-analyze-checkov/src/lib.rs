//! The analyzer for [Checkov](https://www.checkov.io/).
//!
//! `meridian-analyze-checkov` runs `checkov -d <dir> -o json` over a
//! stage's compiled files, with the framework that fits the stage's target,
//! and reports each failed check as a finding: the check id as the rule,
//! the check's name as the message, the file and line range it points at,
//! and the Meridian resource id, which is the last part of the resource
//! Checkov names (`aws_s3_bucket.logs`).
//!
//! Checkov gives a check a severity only with a Prisma Cloud key, so a
//! failed check without one is reported at the severity the setting
//! `default-severity` names, `high` by default: Checkov itself fails a run
//! on any failed check, and this keeps that meaning under Meridian's
//! `--fail-on high`.
//!
//! The program is `checkov` on the `PATH`, or whatever `MERIDIAN_CHECKOV`
//! names.
//!
//! Settings (`--analyze-config checkov.KEY=VALUE`):
//!
//! | Key | Meaning |
//! |---|---|
//! | `framework` | the Checkov frameworks to run, comma-separated, in place of the ones chosen from the target |
//! | `check` | run only these checks (`--check`), comma-separated |
//! | `skip-check` | skip these checks (`--skip-check`), comma-separated |
//! | `default-severity` | the severity of a failed check Checkov gives none: `high` unless set |
//! | `args` | more arguments for `checkov`, split on whitespace |

use meridian_analyze::{
    program, relative_path, resource_id, run_tool, severity, Answer, Finding, Input,
};
use meridian_value::{decode_value, Value};

/// The variable that names the `checkov` program.
pub const CHECKOV_VARIABLE: &str = "MERIDIAN_CHECKOV";

/// The Checkov frameworks a target's compiled files are scanned with. A
/// target not here is scanned with every framework Checkov has, which
/// finds nothing in a format Checkov does not read.
pub fn frameworks(target: &str) -> Vec<&'static str> {
    match target {
        "terraform" | "opentofu" => vec!["terraform_json"],
        "cloudformation" => vec!["cloudformation"],
        "kubernetes" => vec!["kubernetes"],
        "helm" => vec!["helm", "kubernetes"],
        "bicep" => vec!["bicep"],
        "arm" => vec!["arm"],
        "ansible" => vec!["ansible"],
        "docker" | "podman" => vec!["dockerfile"],
        _ => Vec::new(),
    }
}

/// The arguments `checkov` is run with for `input`.
pub fn arguments(input: &Input) -> Vec<String> {
    let mut args = vec![
        "-d".to_owned(),
        ".".to_owned(),
        "-o".to_owned(),
        "json".to_owned(),
        "--quiet".to_owned(),
        "--compact".to_owned(),
        // Meridian decides what fails; Checkov's own exit code would say
        // "failed checks" where Meridian asks "could you look".
        "--soft-fail".to_owned(),
    ];
    let framework = input.setting("framework");
    let frameworks: Vec<String> = if framework.is_empty() {
        frameworks(&input.target)
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        framework
            .split(',')
            .map(|f| f.trim().to_owned())
            .filter(|f| !f.is_empty())
            .collect()
    };
    if !frameworks.is_empty() {
        args.push("--framework".to_owned());
        args.extend(frameworks);
    }
    for (setting, flag) in [("check", "--check"), ("skip-check", "--skip-check")] {
        let value = input.setting(setting);
        if !value.is_empty() {
            args.push(flag.to_owned());
            args.push(value);
        }
    }
    args.extend(input.extra_args());
    args
}

/// Runs Checkov over the stage and translates what it found.
pub fn analyze(input: &Input) -> Result<Answer, String> {
    let checkov = program(CHECKOV_VARIABLE, "checkov");
    let output = run_tool(&checkov, &arguments(input), CHECKOV_VARIABLE, &[0, 1])?;
    translate(&output.stdout, input)
}

/// Turns Checkov's JSON report into the findings.
pub fn translate(stdout: &[u8], input: &Input) -> Result<Answer, String> {
    let default = {
        let word = input.setting("default-severity");
        if word.is_empty() {
            "high".to_owned()
        } else {
            severity(&word, "high")
        }
    };
    if stdout.iter().all(u8::is_ascii_whitespace) {
        // Checkov prints nothing at all when no framework found a file.
        return Ok(Answer::default());
    }
    let value = decode_value(stdout).map_err(|e| format!("checkov did not answer in JSON: {e}"))?;
    // One framework answers with an object; several with a list of them.
    let reports: Vec<&Value> = match &value {
        Value::List(list) => list.iter().collect(),
        other => vec![other],
    };
    let mut answer = Answer::default();
    for report in reports {
        let Some(map) = report.as_map() else { continue };
        let Some(failed) = map
            .get("results")
            .and_then(Value::as_map)
            .and_then(|r| r.get("failed_checks"))
            .and_then(Value::as_list)
        else {
            continue;
        };
        for check in failed {
            let Some(check) = check.as_map() else {
                continue;
            };
            let text = |key: &str| -> String {
                match check.get(key) {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                }
            };
            let rule = text("check_id");
            if rule.is_empty() {
                continue;
            }
            let line = check
                .get("file_line_range")
                .and_then(Value::as_list)
                .and_then(|range| range.first())
                .and_then(|n| match n {
                    Value::Uint(n) => Some(*n),
                    Value::Int(n) => u64::try_from(*n).ok(),
                    _ => None,
                })
                .unwrap_or(0);
            answer.findings.push(Finding {
                rule,
                severity: severity(&text("severity"), &default),
                message: text("check_name"),
                file: relative_path(&text("file_path"), &input.dir),
                line,
                resource: resource_id(&text("resource")),
                url: text("guideline"),
            });
        }
    }
    Ok(answer)
}
