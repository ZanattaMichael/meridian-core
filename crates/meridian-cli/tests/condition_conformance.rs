//! A condition means the same on every target.
//!
//! Each target has an example of its own under `examples/<target>/`. Here
//! every resource and output of that example is given the same `when`, and
//! the example is compiled by the real command with the real plugin, once for
//! each row of a table of conditions and data. Whatever the target, the row
//! must be decided the same way:
//!
//! - true: the files are, byte for byte, the ones the example compiles to
//!   with no condition in it;
//! - false: every resource and output is reported removed, by that condition
//!   in the words the document used;
//! - cannot be decided: the same refusal, in the same words.
//!
//! The same is asked of a `when` on the whole document. Conditions are
//! decided by Meridian before a plugin is started, so this holds by
//! construction today. The test is here so that it cannot stop holding
//! without someone noticing: a target that began to read a condition itself,
//! or was handed one undecided, would fail a row.

#![cfg(unix)]

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_plugin_testkit::{plugin_binary, repo_root, scratch};

/// What a condition must come to with its data.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Decided {
    True,
    False,
    /// Refused, with a message that holds this.
    Refused(&'static str),
}

struct Row {
    what: &'static str,
    condition: &'static str,
    /// The data file, as YAML.
    data: &'static str,
    decided: Decided,
}

const DATA: &str = "env: prod\nreplicas: 3\nregion: eu-west-1\ntags: [web, blue]\nfeatures:\n  \
                    tls: true\n  cache: false\nversion: 2.4.1\nowner: null\n";

/// One row for each kind of thing a condition can do. The whole language is
/// tested row by row in `conditions.rs` against one target; these are the
/// kinds, against all of them.
const ROWS: &[Row] = &[
    Row {
        what: "text compared, true",
        condition: "data.env == 'prod'",
        data: DATA,
        decided: Decided::True,
    },
    Row {
        what: "text compared, false",
        condition: "data.env != 'prod'",
        data: DATA,
        decided: Decided::False,
    },
    Row {
        what: "a number compared",
        condition: "data.replicas >= 3 && data.replicas < 10",
        data: DATA,
        decided: Decided::True,
    },
    Row {
        what: "and, or, not and parentheses",
        condition: "!(data.env == 'dev' || data.features.cache) && data.features.tls",
        data: DATA,
        decided: Decided::True,
    },
    Row {
        what: "a key that is set",
        condition: "defined(data.features.tls) && !defined(data.features.missing)",
        data: DATA,
        decided: Decided::True,
    },
    Row {
        what: "membership of a list",
        condition: "'green' in data.tags",
        data: DATA,
        decided: Decided::False,
    },
    Row {
        what: "a pattern",
        condition: "data.region =~ '^eu-' && data.env !~ '^d'",
        data: DATA,
        decided: Decided::True,
    },
    Row {
        what: "a function",
        condition: "length(data.tags) == 2 && startsWith(data.region, 'eu')",
        data: DATA,
        decided: Decided::True,
    },
    Row {
        what: "a version compared as a version",
        condition: "semverSatisfies(data.version, '>=2.0.0 <3.0.0')",
        data: DATA,
        decided: Decided::True,
    },
    Row {
        what: "a fallback for a key that is missing",
        condition: "(data.features.debug ?? false) || (data.owner ?? 'nobody') == 'nobody'",
        data: DATA,
        decided: Decided::True,
    },
    Row {
        what: "a choice",
        condition: "(data.env == 'prod' ? data.replicas : 1) == 1",
        data: DATA,
        decided: Decided::False,
    },
    Row {
        what: "a short circuit that never reads the missing key",
        condition: "data.env == 'dev' && data.missing.deeper == 1",
        data: DATA,
        decided: Decided::False,
    },
    Row {
        what: "a key that is not set",
        condition: "data.missing == 'x'",
        data: DATA,
        decided: Decided::Refused("data.missing is not set"),
    },
    Row {
        what: "an answer that is not true or false",
        condition: "data.replicas",
        data: DATA,
        decided: Decided::Refused("true or false"),
    },
    Row {
        what: "types that do not go together",
        condition: "data.env > 3",
        data: DATA,
        decided: Decided::Refused("cannot compare"),
    },
];

/// The targets, each with the first example of its own that holds one
/// document and decides nothing from data.
fn examples() -> Vec<(String, PathBuf)> {
    let root = repo_root();
    let mut found = Vec::new();
    for plugin in fs::read_dir(root.join("plugins")).unwrap() {
        let target = plugin.unwrap().file_name().to_string_lossy().into_owned();
        let Ok(files) = fs::read_dir(root.join("examples").join(&target)) else {
            continue;
        };
        let mut files: Vec<PathBuf> = files
            .map(|file| file.unwrap().path())
            .filter(|file| file.extension().is_some_and(|e| e == "yaml"))
            .collect();
        files.sort();
        let open = ["$", "{{"].concat();
        let plain = files.into_iter().find(|file| {
            let text = fs::read_to_string(file).unwrap();
            let documents = text
                .lines()
                .filter(|line| line.starts_with("  target: "))
                .count();
            documents == 1
                && !text.contains(&open)
                && !text
                    .lines()
                    .any(|line| line.trim_start().starts_with("when:"))
        });
        if let Some(file) = plain {
            found.push((target, file));
        }
    }
    found.sort();
    found
}

/// A condition as a document writes it: a JSON string is also a YAML one,
/// so no row has to think about YAML's quoting.
fn quoted(condition: &str) -> String {
    meridian_sdk::Value::String(condition.to_owned()).canonical()
}

/// The example with `when: <condition>` on every resource and every output,
/// and how many were given one.
fn on_everything(example: &str, condition: &str) -> (String, usize) {
    let mut out = String::new();
    let mut section = "";
    let mut given = 0;
    for line in example.lines() {
        if let Some(rest) = line.strip_prefix("  ") {
            if !rest.starts_with([' ', '#', '-']) {
                section = rest.split(':').next().unwrap_or("");
            }
        }
        out.push_str(line);
        out.push('\n');
        let entry = match section {
            "resources" => line.starts_with("    - id: "),
            "outputs" => line.starts_with("    - name: "),
            _ => false,
        };
        if entry {
            out.push_str(&format!("      when: {}\n", quoted(condition)));
            given += 1;
        }
    }
    (out, given)
}

/// The example with `when: <condition>` on the document itself.
fn on_the_document(example: &str, condition: &str) -> String {
    let mut out = String::new();
    let mut given = false;
    for line in example.lines() {
        out.push_str(line);
        out.push('\n');
        if !given && line.starts_with("  target: ") {
            out.push_str(&format!("  when: {}\n", quoted(condition)));
            given = true;
        }
    }
    assert!(given, "no spec.target to write a when beside");
    out
}

/// What a compile came to: whether it succeeded, what it said, and every
/// file it wrote.
struct Compiled {
    succeeded: bool,
    said: String,
    files: BTreeMap<String, Vec<u8>>,
}

fn compile(target: &str, dir: &Path, document: &str, data: Option<&str>) -> Compiled {
    let _ = fs::remove_dir_all(dir.join("out"));
    fs::write(dir.join("doc.yaml"), document).unwrap();
    let binary = plugin_binary(&format!("meridian-target-{target}"));
    let mut meridian = Command::new(env!("CARGO_BIN_EXE_meridian"));
    meridian
        .args(["compile", "doc.yaml", "--out", "out", "--plugin-dir"])
        .arg(binary.parent().unwrap())
        .current_dir(dir);
    if let Some(data) = data {
        fs::write(dir.join("data.yaml"), data).unwrap();
        meridian.args(["--data", "data.yaml"]);
    }
    let output = meridian.output().expect("meridian runs");
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.join("out")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let name = path.strip_prefix(dir.join("out")).unwrap();
                files.insert(
                    name.to_string_lossy().into_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    Compiled {
        succeeded: output.status.success(),
        said: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        files,
    }
}

/// The refusal a compile printed, without the file, the line and the
/// resource it was found at, which differ from one example to the next.
fn refusal(said: &str) -> Option<String> {
    let line = said.lines().find(|line| line.contains("FAILED: "))?;
    let after = line.split("when: ").nth(1)?;
    let after = match after.strip_prefix("resource \"") {
        Some(rest) => rest.split_once("\": ")?.1,
        None => after,
    };
    Some(after.to_owned())
}

/// Every row against one target. Returns what went wrong, and for each row
/// that must be refused, the words it was refused in.
fn one_target(target: &str, file: &Path) -> (Vec<String>, BTreeMap<&'static str, String>) {
    let mut failures = Vec::new();
    let mut refusals = BTreeMap::new();
    let example = fs::read_to_string(file).unwrap();
    let dir = scratch(
        env!("CARGO_TARGET_TMPDIR"),
        &format!("condition-conformance-{target}"),
    );
    let plain = compile(target, &dir, &example, None);
    assert!(
        plain.succeeded && !plain.files.is_empty(),
        "{target}: {}",
        plain.said
    );
    let (_, entries) = on_everything(&example, "true");
    assert!(entries > 0, "{target}: nothing to put a condition on");

    for row in ROWS {
        let at = format!("{target}: {} ({})", row.what, row.condition);
        let (document, _) = on_everything(&example, row.condition);
        let got = compile(target, &dir, &document, Some(row.data));
        match row.decided {
            Decided::True => {
                if !got.succeeded || got.files != plain.files {
                    failures.push(format!(
                        "{at}: true must compile to the example's own files:\n{}",
                        got.said
                    ));
                }
                if got.said.contains("removed") {
                    failures.push(format!("{at}: something was removed:\n{}", got.said));
                }
            }
            Decided::False => {
                let removed = got
                    .said
                    .lines()
                    .filter(|line| line.ends_with(&format!(": {} is false", row.condition)))
                    .count();
                if removed != entries || !got.said.contains(&format!("when: {entries} removed")) {
                    failures.push(format!(
                        "{at}: false must remove all {entries}, and {removed} were:\n{}",
                        got.said
                    ));
                }
            }
            Decided::Refused(why) => match refusal(&got.said) {
                Some(said) if !got.succeeded && said.contains(why) && got.files.is_empty() => {
                    refusals.insert(row.what, said);
                }
                _ => failures.push(format!("{at}: must be refused with {why:?}:\n{}", got.said)),
            },
        }
    }

    // The same condition on the whole document, for one row of each answer.
    let mut answers = HashSet::new();
    for row in ROWS {
        if !answers.insert(std::mem::discriminant(&row.decided)) {
            continue;
        }
        let at = format!("{target}: spec.when: {}", row.condition);
        let document = on_the_document(&example, row.condition);
        let got = compile(target, &dir, &document, Some(row.data));
        let right = match row.decided {
            Decided::True => got.succeeded && got.files == plain.files,
            Decided::False => {
                got.succeeded
                    && got.files.is_empty()
                    && got
                        .said
                        .contains(&format!("skipped: its when ({}) is false", row.condition))
            }
            Decided::Refused(why) => {
                !got.succeeded && got.files.is_empty() && got.said.contains(why)
            }
        };
        if !right {
            failures.push(format!("{at}: not {:?}:\n{}", row.decided, got.said));
        }
    }
    (failures, refusals)
}

#[test]
fn every_target_decides_every_condition_the_same_way() {
    let examples = examples();
    let targets = fs::read_dir(repo_root().join("plugins")).unwrap().count();
    assert_eq!(
        examples.len(),
        targets,
        "every target needs an example of one document that decides nothing from data: {:?}",
        examples
            .iter()
            .map(|(target, _)| target)
            .collect::<Vec<_>>()
    );

    // One thread a target: each starts its own plugin a few dozen times.
    let results: Vec<(String, Vec<String>, BTreeMap<&'static str, String>)> =
        std::thread::scope(|scope| {
            let running: Vec<_> = examples
                .iter()
                .map(|(target, file)| {
                    scope.spawn(move || {
                        let (failures, refusals) = one_target(target, file);
                        (target.clone(), failures, refusals)
                    })
                })
                .collect();
            running
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect()
        });

    let failures: Vec<&String> = results
        .iter()
        .flat_map(|(_, failures, _)| failures)
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");

    // A condition that cannot be decided is refused in the same words
    // whichever target the document names.
    let (first, _, wording) = &results[0];
    for (target, _, refusals) in &results {
        assert_eq!(
            refusals, wording,
            "{target} and {first} refuse a condition in different words"
        );
    }
    let refused = ROWS
        .iter()
        .filter(|row| matches!(row.decided, Decided::Refused(_)))
        .count();
    assert_eq!(wording.len(), refused);
}
