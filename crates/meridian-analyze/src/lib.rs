//! What every analyzer program shares.
//!
//! An analyzer is `meridian-analyze-<name>`: Meridian starts it in a
//! directory holding one stage's compiled files, hands it the [`Input`] on
//! standard input, and reads an [`Answer`] on standard output. This crate
//! reads the one and writes the other, and runs the scanner in between, so
//! that an analyzer for a new tool is the translation of that tool's output
//! and nothing else. The protocol is documented in
//! `docs/reference/analyzers.md`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output};

use meridian_value::{decode_value, Value};

/// What Meridian tells an analyzer about the stage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Input {
    /// `compile`, `plan` or `apply`.
    pub mode: String,
    pub run: String,
    /// The document's `metadata.name`.
    pub stage: String,
    /// `Infrastructure` or `ResourceSet`.
    pub kind: String,
    /// The document's target, such as `terraform`.
    pub target: String,
    /// The target's category, such as `infrastructure-as-code`.
    pub category: String,
    pub host: String,
    pub labels: BTreeMap<String, String>,
    /// Where the compiled files are. The program is started there too.
    pub dir: PathBuf,
    /// The compiled files, as paths inside `dir`.
    pub files: Vec<String>,
    /// The analyzer's settings, as given with `--analyze-config`.
    pub settings: BTreeMap<String, String>,
}

impl Input {
    /// Reads the input from standard input.
    pub fn read() -> Result<Input, String> {
        let mut raw = Vec::new();
        std::io::stdin()
            .read_to_end(&mut raw)
            .map_err(|e| format!("reading the input: {e}"))?;
        Input::parse(&raw)
    }

    /// Reads the input from its JSON.
    pub fn parse(raw: &[u8]) -> Result<Input, String> {
        let value = decode_value(raw).map_err(|e| format!("the input is not JSON: {e}"))?;
        let top = value
            .as_map()
            .ok_or_else(|| "the input is not an object".to_owned())?;
        let section = |name: &str| -> BTreeMap<String, Value> {
            top.get(name)
                .and_then(Value::as_map)
                .cloned()
                .unwrap_or_default()
        };
        let text = |map: &BTreeMap<String, Value>, key: &str| -> String {
            match map.get(key) {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            }
        };
        let strings = |map: &BTreeMap<String, Value>| -> BTreeMap<String, String> {
            map.iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                .collect()
        };
        let run = section("run");
        let stage = section("stage");
        let artifact = section("artifact");
        Ok(Input {
            mode: text(&run, "mode"),
            run: text(&run, "id"),
            stage: text(&stage, "name"),
            kind: text(&stage, "kind"),
            target: text(&stage, "target"),
            category: text(&stage, "category"),
            host: text(&stage, "host"),
            labels: stage
                .get("labels")
                .and_then(Value::as_map)
                .map(strings)
                .unwrap_or_default(),
            dir: PathBuf::from(text(&artifact, "dir")),
            files: artifact
                .get("files")
                .and_then(Value::as_list)
                .map(|list| {
                    list.iter()
                        .filter_map(|f| f.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            settings: top
                .get("settings")
                .and_then(Value::as_map)
                .map(strings)
                .unwrap_or_default(),
        })
    }

    /// A setting, from the input or from the environment Meridian also puts
    /// it in (`MERIDIAN_ANALYZE_<KEY>`), whichever is set; empty when
    /// neither is.
    pub fn setting(&self, key: &str) -> String {
        if let Some(value) = self.settings.get(key) {
            return value.clone();
        }
        let mut name = String::from("MERIDIAN_ANALYZE_");
        name.extend(key.chars().map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        }));
        std::env::var(name).unwrap_or_default()
    }

    /// Extra arguments for the scanner, from the setting `args`, split on
    /// whitespace.
    pub fn extra_args(&self) -> Vec<String> {
        self.setting("args")
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }
}

/// One thing the analyzer found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Finding {
    pub rule: String,
    /// `critical`, `high`, `medium`, `low` or `info`.
    pub severity: String,
    pub message: String,
    /// The compiled file, as a path inside the stage's directory.
    pub file: String,
    pub line: u64,
    /// The Meridian resource id, when the scanner's resource name gives it.
    pub resource: String,
    pub url: String,
}

/// A number the analyzer measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measure {
    pub name: String,
    pub value: f64,
    pub unit: String,
}

/// What the analyzer answers with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    pub findings: Vec<Finding>,
    pub measures: Vec<Measure>,
}

impl Answer {
    /// The answer as the JSON Meridian reads.
    pub fn to_json(&self) -> String {
        let text = |s: &str| Value::from(s);
        let object = |fields: Vec<(&str, Value)>| {
            Value::Map(
                fields
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value))
                    .collect(),
            )
        };
        let findings = self
            .findings
            .iter()
            .map(|f| {
                object(vec![
                    ("rule", text(&f.rule)),
                    ("severity", text(&f.severity)),
                    ("message", text(&f.message)),
                    ("file", text(&f.file)),
                    ("line", Value::Uint(f.line)),
                    ("resource", text(&f.resource)),
                    ("url", text(&f.url)),
                ])
            })
            .collect();
        let measures = self
            .measures
            .iter()
            .map(|m| {
                object(vec![
                    ("name", text(&m.name)),
                    ("value", Value::Float(m.value)),
                    ("unit", text(&m.unit)),
                ])
            })
            .collect();
        object(vec![
            ("findings", Value::List(findings)),
            ("measures", Value::List(measures)),
        ])
        .canonical()
    }
}

/// Runs the scanner and returns what it printed. A scanner that cannot be
/// started is an error that says how to name its program; one that exits
/// with a status not in `fine` is an error quoting its last line.
pub fn run_tool(
    program: &str,
    args: &[String],
    variable: &str,
    fine: &[i32],
) -> Result<Output, String> {
    let output = Command::new(program).args(args).output().map_err(|e| {
        format!("starting {program}: {e}. Install it, or name its program in {variable}")
    })?;
    match output.status.code() {
        Some(code) if fine.contains(&code) => Ok(output),
        Some(code) => Err(format!(
            "{program} exited with status {code}{}",
            last_line(&String::from_utf8_lossy(&output.stderr))
        )),
        None => Err(format!("{program} was stopped by a signal")),
    }
}

/// The last non-empty line of what a tool said, as a clause.
pub fn last_line(text: &str) -> String {
    match text.lines().rev().find(|line| !line.trim().is_empty()) {
        Some(line) => format!(": {}", line.trim()),
        None => String::new(),
    }
}

/// The program named by `variable`, or `default`.
pub fn program(variable: &str, default: &str) -> String {
    std::env::var(variable)
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// A scanner's severity word as Meridian's: `CRITICAL`, `HIGH`, `MEDIUM`,
/// `LOW`, `INFO` and `UNKNOWN` read as themselves, lower-cased; anything
/// else, or nothing, as `default`.
pub fn severity(word: &str, default: &str) -> String {
    match word.trim().to_ascii_lowercase().as_str() {
        "critical" => "critical",
        "high" | "error" => "high",
        "medium" | "moderate" | "warning" => "medium",
        "low" => "low",
        "info" | "informational" | "note" => "info",
        _ => default,
    }
    .to_owned()
}

/// The Meridian resource id a scanner's resource name ends in: the last
/// dotted part of `aws_s3_bucket.logs` or `Deployment.default.web`, which
/// is the document's id wherever the emitter could keep it. Index parts
/// such as `[0]` are dropped.
pub fn resource_id(name: &str) -> String {
    let last = name.rsplit('.').next().unwrap_or(name);
    let last = last.split('[').next().unwrap_or(last);
    last.trim().to_owned()
}

/// A scanner's file path as a path inside the stage's directory: the
/// directory itself and any leading slash removed.
pub fn relative_path(path: &str, dir: &std::path::Path) -> String {
    let dir_text = dir.display().to_string();
    let mut text = path.replace('\\', "/");
    let dir_text = dir_text.replace('\\', "/");
    if !dir_text.is_empty() {
        if let Some(rest) = text.strip_prefix(&dir_text) {
            text = rest.to_owned();
        }
    }
    text.trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

/// Runs an analyzer's `main`: reads the input, calls `analyze`, prints the
/// answer, and exits 1 with the reason when anything goes wrong.
pub fn main_with(
    name: &str,
    analyze: fn(&Input) -> Result<Answer, String>,
) -> std::process::ExitCode {
    let answer = Input::read().and_then(|input| analyze(&input));
    match answer {
        Ok(answer) => {
            println!("{}", answer.to_json());
            std::process::ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("{name}: {why}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_reads_and_a_setting_comes_from_it_or_the_environment() {
        let input = Input::parse(
            br#"{"run":{"id":"r1","mode":"plan"},"stage":{"name":"site","kind":"Infrastructure","target":"terraform","category":"infrastructure-as-code","host":"","labels":{"env":"prod"}},"artifact":{"dir":"/tmp/x","files":["main.tf.json"]},"settings":{"args":"--quiet  -x"}}"#,
        )
        .unwrap();
        assert_eq!(input.mode, "plan");
        assert_eq!(input.stage, "site");
        assert_eq!(input.target, "terraform");
        assert_eq!(input.labels["env"], "prod");
        assert_eq!(input.dir, PathBuf::from("/tmp/x"));
        assert_eq!(input.files, ["main.tf.json"]);
        assert_eq!(input.extra_args(), ["--quiet", "-x"]);
        assert_eq!(input.setting("missing"), "");
        assert!(Input::parse(b"[]").is_err());
    }

    #[test]
    fn the_answer_is_the_json_meridian_reads() {
        let answer = Answer {
            findings: vec![Finding {
                rule: "R".to_owned(),
                severity: "high".to_owned(),
                message: "m".to_owned(),
                file: "a".to_owned(),
                line: 2,
                resource: "x".to_owned(),
                url: String::new(),
            }],
            measures: vec![Measure {
                name: "n".to_owned(),
                value: 1.5,
                unit: "USD".to_owned(),
            }],
        };
        assert_eq!(
            answer.to_json(),
            r#"{"findings":[{"file":"a","line":2,"message":"m","resource":"x","rule":"R","severity":"high","url":""}],"measures":[{"name":"n","unit":"USD","value":1.5}]}"#
        );
    }

    #[test]
    fn helpers_read_severities_resources_and_paths() {
        assert_eq!(severity("HIGH", "medium"), "high");
        assert_eq!(severity("", "medium"), "medium");
        assert_eq!(severity("UNKNOWN", "low"), "low");
        assert_eq!(resource_id("aws_s3_bucket.logs"), "logs");
        assert_eq!(resource_id("Deployment.default.web"), "web");
        assert_eq!(resource_id("module.x.aws_instance.web[0]"), "web");
        assert_eq!(resource_id("logs"), "logs");
        assert_eq!(
            relative_path("/main.tf.json", std::path::Path::new("/tmp/x")),
            "main.tf.json"
        );
        assert_eq!(
            relative_path("/tmp/x/main.tf.json", std::path::Path::new("/tmp/x")),
            "main.tf.json"
        );
        assert_eq!(
            relative_path("./main.tf.json", std::path::Path::new("")),
            "main.tf.json"
        );
    }
}
