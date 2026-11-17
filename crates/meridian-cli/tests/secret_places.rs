//! Every place a document can ask a question of a secret, through the real
//! command: a `when` on a document, a resource, an output and an edge, a
//! named condition, a variable, and a choice written into a file.
//!
//! Each row is one place. It is planned and applied twice, with a password
//! that makes the question true and one that makes it false, and must give
//! the two answers. `compile`, which fetches nothing, must stop at that place
//! and name it. And the password must be in nothing either run printed or
//! left on disk.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Unlike anything a document or the command writes, so that finding it
/// anywhere is finding a leak.
const STRONG: &str = "Vt9-zzq-not-a-real-password-4kLm";
/// The password the documents compare against. It is the documents' own
/// text, so it is not a secret to look for.
const DEFAULT: &str = "changeme";

struct Place {
    what: &'static str,
    /// `Infrastructure` or `ResourceSet`.
    kind: &'static str,
    /// What follows `spec.secrets` in the document.
    spec: &'static str,
    /// Where `compile` stops, as it names the place.
    stops_at: &'static str,
    /// What a plan shows with a strong password, and with the default one.
    strong: &'static [&'static str],
    weak: &'static [&'static str],
    /// What the other password's plan must not show.
    only_strong: &'static [&'static str],
}

const PLACES: &[Place] = &[
    Place {
        what: "a when on a resource",
        kind: "ResourceSet",
        spec: r#"
  resources:
    - id: always
      type: file
      params:
        content: "always\n"
    - id: guarded
      type: file
      when: secret.password != 'changeme'
      params:
        content: "guarded\n"
"#,
        stops_at: "spec.resources[1].when",
        strong: &["+ create files/always", "+ create files/guarded"],
        weak: &[
            "+ create files/always",
            "resource \"guarded\": secret.password != 'changeme' is false",
        ],
        only_strong: &["+ create files/guarded"],
    },
    Place {
        what: "a when on the whole document",
        kind: "ResourceSet",
        spec: r#"
  when: secret.password != 'changeme'
  resources:
    - id: always
      type: file
      params:
        content: "always\n"
"#,
        stops_at: "spec.when",
        strong: &["+ create files/always"],
        weak: &["skipped: its when (secret.password != 'changeme') is false"],
        only_strong: &["+ create files/always"],
    },
    Place {
        what: "a named condition",
        kind: "ResourceSet",
        spec: r#"
  conditions:
    strong: secret.password != 'changeme'
  resources:
    - id: guarded
      type: file
      when: cond.strong
      params:
        content: "guarded\n"
"#,
        stops_at: "spec.conditions.strong",
        strong: &["+ create files/guarded"],
        weak: &["resource \"guarded\": cond.strong is false"],
        only_strong: &["+ create files/guarded"],
    },
    Place {
        what: "a named condition that reads another",
        kind: "ResourceSet",
        spec: r#"
  conditions:
    is_default: secret.password == 'changeme'
    strong: "!cond.is_default && secret.password != ''"
  resources:
    - id: guarded
      type: file
      when: cond.strong
      params:
        content: "guarded\n"
"#,
        stops_at: "spec.conditions.is_default",
        strong: &["+ create files/guarded"],
        weak: &["resource \"guarded\": cond.strong is false"],
        only_strong: &["+ create files/guarded"],
    },
    Place {
        what: "a variable that holds the answer",
        kind: "ResourceSet",
        spec: r#"
  variables:
    strong: ${{ secret.password != 'changeme' }}
  resources:
    - id: guarded
      type: file
      when: var.strong
      params:
        content: "guarded\n"
"#,
        stops_at: "spec.variables.strong",
        strong: &["+ create files/guarded"],
        weak: &["resource \"guarded\": var.strong is false"],
        only_strong: &["+ create files/guarded"],
    },
    Place {
        what: "a when on an output",
        kind: "Infrastructure",
        spec: r#"
  resources:
    - id: always
      type: file
      params:
        content: "always\n"
  outputs:
    - name: shown
      when: secret.password != 'changeme'
      value: ${always.content}
"#,
        stops_at: "spec.outputs[0].when",
        strong: &["+ create files/always", "+ create outputs/shown"],
        weak: &[
            "+ create files/always",
            "output \"shown\": secret.password != 'changeme' is false",
        ],
        only_strong: &["+ create outputs/shown"],
    },
    Place {
        what: "a when on a dependsOn entry",
        kind: "ResourceSet",
        spec: r#"
  resources:
    - id: first
      type: file
      params:
        content: "first\n"
    - id: second
      type: file
      params:
        content: "second\n"
      dependsOn:
        - resource: first
          when: secret.password != 'changeme'
"#,
        stops_at: "spec.resources[1].dependsOn[0].when",
        strong: &["+ create files/first", "+ create files/second"],
        weak: &[
            "+ create files/first",
            "+ create files/second",
            "\"second\" depends on \"first\": secret.password != 'changeme' is false",
        ],
        only_strong: &[],
    },
    Place {
        what: "a when on a notifies entry",
        kind: "ResourceSet",
        spec: r#"
  resources:
    - id: first
      type: file
      params:
        content: "first\n"
    - id: second
      type: file
      params:
        content: "second\n"
      notifies:
        - resource: first
          action: reload
          when: secret.password != 'changeme'
"#,
        stops_at: "spec.resources[1].notifies[0].when",
        strong: &["+ create files/first", "+ create files/second"],
        weak: &[
            "+ create files/first",
            "+ create files/second",
            "\"second\" notifies \"first\": secret.password != 'changeme' is false",
        ],
        only_strong: &[],
    },
    Place {
        what: "a choice written into a file",
        kind: "ResourceSet",
        // The answer is not a secret, so it can be written. Quoted because
        // the expression holds ": ".
        spec: r#"
  resources:
    - id: report
      type: file
      params:
        content: "password: ${{ secret.password == 'changeme' ? 'default' : 'changed' }}\n"
"#,
        stops_at: "spec.resources[0].params.content",
        strong: &["+ create files/report: 18 bytes"],
        weak: &["+ create files/report: 18 bytes"],
        only_strong: &[],
    },
    Place {
        what: "the answer of a named condition written into a file",
        kind: "ResourceSet",
        spec: r#"
  conditions:
    strong: secret.password != 'changeme'
  resources:
    - id: report
      type: file
      params:
        content: "strong = ${{ cond.strong }}\n"
"#,
        stops_at: "spec.conditions.strong",
        // "strong = true\n" and "strong = false\n".
        strong: &["+ create files/report: 14 bytes"],
        weak: &["+ create files/report: 15 bytes"],
        only_strong: &["files/report: 14 bytes"],
    },
    Place {
        what: "a secret beside data in one condition",
        kind: "ResourceSet",
        spec: r#"
  resources:
    - id: guarded
      type: file
      when: data.env == 'prod' && secret.password != 'changeme'
      params:
        content: "guarded\n"
"#,
        stops_at: "spec.resources[0].when",
        strong: &["+ create files/guarded"],
        weak: &[
            "resource \"guarded\": data.env == 'prod' && secret.password != 'changeme' is false",
        ],
        only_strong: &["+ create files/guarded"],
    },
    Place {
        what: "an optional secret that is not there, with a fallback",
        kind: "ResourceSet",
        spec: r#"
  resources:
    - id: guarded
      type: file
      when: (secret.absent ?? secret.password) != 'changeme' && secret.absent == null
      params:
        content: "guarded\n"
"#,
        stops_at: "spec.resources[0].when",
        strong: &["+ create files/guarded"],
        weak: &["resource \"guarded\""],
        only_strong: &["+ create files/guarded"],
    },
];

fn document(place: &Place) -> String {
    let host = if place.kind == "ResourceSet" {
        "\n  targetHost:\n    from: inline\n    host: web01"
    } else {
        ""
    };
    format!(
        "apiVersion: meridian/v1\nkind: {}\nmetadata:\n  name: site\nspec:\n  target: fake{host}\n  \
         secrets:\n    - name: password\n      from: env\n      key: PLACES_PASSWORD\n    \
         - name: absent\n      from: env\n      key: PLACES_NOT_SET_ANYWHERE\n      optional: true{}",
        place.kind, place.spec
    )
}

fn scratch(index: usize) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secret-places-{index}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn meridian(dir: &Path, command: &str, password: Option<&str>) -> Output {
    let mut meridian = Command::new(env!("CARGO_BIN_EXE_meridian"));
    meridian.args([command, "site.yaml"]);
    if command != "validate" {
        meridian.args(["--set", "env=prod"]);
    }
    meridian
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("PLACES_PASSWORD")
        .env_remove("PLACES_NOT_SET_ANYWHERE");
    if let Some(password) = password {
        meridian.env("PLACES_PASSWORD", password);
    }
    meridian.output().expect("meridian runs")
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Every file under `dir` other than the document, as bytes.
fn left_behind(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if !path.ends_with("site.yaml") {
                found.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    found
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn a_secret_gives_its_answer_in_every_place_a_question_can_be_asked() {
    for (index, place) in PLACES.iter().enumerate() {
        let dir = scratch(index);
        fs::write(dir.join("site.yaml"), document(place)).unwrap();

        let strong = meridian(&dir, "plan", Some(STRONG));
        let text = said(&strong);
        assert!(strong.status.success(), "{}:\n{text}", place.what);
        for expected in place.strong {
            assert!(
                text.contains(expected),
                "{}: no {expected:?} in:\n{text}",
                place.what
            );
        }

        let weak = meridian(&dir, "plan", Some(DEFAULT));
        let text = said(&weak);
        assert!(weak.status.success(), "{}:\n{text}", place.what);
        for expected in place.weak {
            assert!(
                text.contains(expected),
                "{}: no {expected:?} in:\n{text}",
                place.what
            );
        }
        for unexpected in place.only_strong {
            assert!(
                !text.contains(unexpected),
                "{}: {unexpected:?} in:\n{text}",
                place.what
            );
        }
    }
}

#[test]
fn compile_stops_at_the_place_that_reads_a_secret_and_names_it() {
    for (index, place) in PLACES.iter().enumerate() {
        let dir = scratch(100 + index);
        fs::write(dir.join("site.yaml"), document(place)).unwrap();
        // The password is there to be read, and compile does not read it.
        let compile = meridian(&dir, "compile", Some(STRONG));
        let text = said(&compile);
        assert_eq!(compile.status.code(), Some(1), "{}:\n{text}", place.what);
        assert!(
            text.contains(&format!("{}: ", place.stops_at)),
            "{}: not stopped at {} in:\n{text}",
            place.what,
            place.stops_at
        );
        assert!(
            text.contains("(secret password) is not fetched here")
                || text.contains("(secret absent) is not fetched here"),
            "{}:\n{text}",
            place.what
        );

        // validate reads no secret either, and has nothing to say.
        let validate = meridian(&dir, "validate", None);
        assert!(
            validate.status.success(),
            "{}:\n{}",
            place.what,
            said(&validate)
        );
    }
}

#[test]
fn the_secret_is_in_nothing_a_run_prints_or_leaves_behind() {
    for (index, place) in PLACES.iter().enumerate() {
        let dir = scratch(200 + index);
        fs::write(dir.join("site.yaml"), document(place)).unwrap();
        for command in ["plan", "apply", "plan"] {
            let run = meridian(&dir, command, Some(STRONG));
            let text = said(&run);
            assert!(run.status.success(), "{} ({command}):\n{text}", place.what);
            assert!(
                !text.contains(STRONG),
                "{} ({command}):\n{text}",
                place.what
            );
        }
        let left = left_behind(&dir);
        assert!(
            !left.is_empty(),
            "{}: the run left no state to search",
            place.what
        );
        for (path, bytes) in left {
            assert!(
                !holds(&bytes, STRONG),
                "{}: {} holds the secret",
                place.what,
                path.display()
            );
        }
    }
}

/// With no password to read, every row stops before anything is planned,
/// and says where the secret was looked for and nothing more.
#[test]
fn a_secret_that_is_not_there_stops_every_place_the_same_way() {
    for (index, place) in PLACES.iter().enumerate() {
        let dir = scratch(300 + index);
        fs::write(dir.join("site.yaml"), document(place)).unwrap();
        let plan = meridian(&dir, "plan", None);
        let text = said(&plan);
        assert_eq!(plan.status.code(), Some(1), "{}:\n{text}", place.what);
        assert!(
            text.contains(
                "spec.secrets: secret \"password\" (from: env): the environment variable \
                 PLACES_PASSWORD is not set"
            ),
            "{}:\n{text}",
            place.what
        );
        assert!(!text.contains("+ create"), "{}:\n{text}", place.what);
    }
}
