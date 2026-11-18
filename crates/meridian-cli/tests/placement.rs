//! What a plugin is given when an interpolation places a value from data:
//! one table, through the real command and the fake target, which writes
//! each resource's `content` to a file exactly as it was handed over.
//!
//! The rule the table holds: what the author wrote in the document is a
//! reference, and what the data supplied never becomes one, however it got
//! there. The targets' half of the rule, writing the escape out as the
//! tool's own literal, is held by
//! `text_given_as_a_literal_is_never_a_reference_on_any_target` in
//! `conformance.rs`.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("placement-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// One row: what the document says, what the data says, and what the plugin
/// must be given.
struct Row {
    what: &'static str,
    /// The `content` param, as YAML: quote it where YAML needs it.
    content: &'static str,
    /// Lines under `spec.variables`, or nothing.
    variables: &'static str,
    /// The data file.
    data: &'static str,
    given: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        what: "text from data that looks like a shell variable",
        content: "cd ${{ data.dir }}",
        variables: "",
        data: "dir: ${HOME}/bin\n",
        given: "cd $${HOME}/bin",
    },
    Row {
        what: "text from data that names a real resource and param",
        content: "see ${{ data.note }}",
        variables: "",
        data: "note: ${other.content}\n",
        given: "see $${other.content}",
    },
    Row {
        what: "a reference the author wrote is left as a reference",
        content: "see ${other.content}",
        variables: "",
        data: "unused: 1\n",
        given: "see ${other.content}",
    },
    Row {
        what: "a reference and a placed look-alike side by side",
        content: "${other.content} and ${{ data.note }}",
        variables: "",
        data: "note: ${other.content}\n",
        given: "${other.content} and $${other.content}",
    },
    Row {
        what: "the whole value from data",
        content: "${{ data.dir }}",
        variables: "",
        data: "dir: ${HOME}/bin\n",
        given: "$${HOME}/bin",
    },
    Row {
        what: "through a variable",
        content: "cd ${{ var.dir }}",
        variables: "    dir: ${{ data.dir }}\n",
        data: "dir: ${HOME}/bin\n",
        given: "cd $${HOME}/bin",
    },
    Row {
        what: "through a variable built from two values",
        content: "${{ var.both }}",
        variables: "    both: ${{ data.a }}-${{ data.b }}\n",
        data: "a: ${x.y}\nb: ${z.w}\n",
        given: "$${x.y}-$${z.w}",
    },
    Row {
        what: "joined out of a list",
        content: "${{ join(data.dirs, ':') }}",
        variables: "",
        data: "dirs:\n  - ${HOME}/bin\n  - /usr/bin\n",
        given: "$${HOME}/bin:/usr/bin",
    },
    Row {
        what: "a dollar from data that meets a brace the author wrote",
        content: "${{ data.sign }}{other.content}",
        variables: "",
        data: "sign: $\n",
        given: "$${other.content}",
    },
    Row {
        what: "two values from data that would meet as a reference",
        content: "${{ data.sign }}${{ data.rest }}",
        variables: "",
        data: "sign: $\nrest: \"{other.content}\"\n",
        given: "$${other.content}",
    },
    Row {
        what: "a dollar from data that meets nothing",
        content: "costs ${{ data.sign }}5",
        variables: "",
        data: "sign: $\n",
        given: "costs $5",
    },
    Row {
        what: "a dollar with no brace after it",
        content: "${{ data.home }}/bin",
        variables: "",
        data: "home: $HOME\n",
        given: "$HOME/bin",
    },
    Row {
        what: "a number placed in text is not escaped",
        content: "listen ${{ data.port }}",
        variables: "",
        data: "port: 8080\n",
        given: "listen 8080",
    },
    Row {
        what: "another tool's own syntax from data is passed as it is",
        content: "${{ data.tpl }}",
        variables: "",
        data: "tpl: \"{{ ansible_hostname }} %{ if x } #{y}\"\n",
        given: "{{ ansible_hostname }} %{ if x } #{y}",
    },
    // Read by a plugin, `$$${x.y}` is a dollar and then the literal `${x.y}`:
    // the text the data held, dollar for dollar.
    Row {
        what: "text from data that already holds the escape keeps both dollars",
        content: "${{ data.v }}",
        variables: "",
        data: "v: $${x.y}\n",
        given: "$$${x.y}",
    },
    Row {
        what: "two dollars from data before a brace the author wrote",
        content: "${{ data.v }}{other.content}",
        variables: "",
        data: "v: $$\n",
        given: "$$${other.content}",
    },
    Row {
        what: "a dollar from data before a literal the author escaped",
        content: "${{ data.v }}$${keep}",
        variables: "",
        data: "v: $\n",
        given: "$$${keep}",
    },
    Row {
        what: "a dollar from data before text that would open an interpolation",
        content: "${{ data.sign }}{{ x }}",
        variables: "",
        data: "sign: $\n",
        given: "$${{ x }}",
    },
    Row {
        what: "two halves with an empty value between them",
        content: "${{ data.a }}${{ data.b }}${{ data.c }}",
        variables: "",
        data: "a: $\nb: ''\nc: \"{x.y}\"\n",
        given: "$${x.y}",
    },
    Row {
        what: "text from data inside a mapping turned into JSON",
        content: "${{ toJson(data.m) }}",
        variables: "",
        data: "m:\n  k: ${x.y}\n",
        given: "{\"k\":\"$${x.y}\"}",
    },
    Row {
        what: "text from data after a function has changed it",
        content: "${{ upper(data.v) }}",
        variables: "",
        data: "v: ${x.y}\n",
        given: "$${X.Y}",
    },
    Row {
        what: "an interpolation in the data is text, and is not worked out",
        content: "${{ data.v }}",
        variables: "",
        data: "v: \"${{ data.w }}\"\nw: 1\n",
        given: "$${{ data.w }}",
    },
    Row {
        what: "an interpolation the author escaped is handed over as written",
        content: "$${{ data.v }}",
        variables: "",
        data: "v: 1\n",
        given: "$${{ data.v }}",
    },
    Row {
        what: "a value that ends in a dollar, with nothing after it",
        content: "${{ data.v }}",
        variables: "",
        data: "v: \"a$\"\n",
        given: "a$",
    },
];

fn document(row: &Row) -> String {
    let variables = if row.variables.is_empty() {
        String::new()
    } else {
        format!("  variables:\n{}", row.variables)
    };
    format!(
        "apiVersion: meridian/v1\n\
         kind: ResourceSet\n\
         metadata:\n  name: site\n\
         spec:\n  target: fake\n  targetHost:\n    from: inline\n    host: h\n\
         {variables}  resources:\n    \
         - id: other\n      type: file\n      params:\n        content: other\n    \
         - id: placed\n      type: file\n      params:\n        content: \"{}\"\n",
        row.content.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// Compiles one row and returns what the fake target was given for the
/// `placed` resource, or what the command said when it failed.
fn given(index: usize, row: &Row) -> Result<String, String> {
    let dir = scratch(&format!("row-{index}"));
    fs::write(dir.join("site.yaml"), document(row)).unwrap();
    fs::write(dir.join("data.yaml"), row.data).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args([
            "compile",
            "site.yaml",
            "--data",
            "data.yaml",
            "--out",
            "build",
        ])
        .current_dir(&dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .output()
        .expect("meridian runs");
    if !output.status.success() {
        return Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    fs::read_to_string(dir.join("build/site/files/placed")).map_err(|e| e.to_string())
}

#[test]
fn what_data_supplies_is_given_to_the_plugin_as_a_literal() {
    let mut failures = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        match given(index, row) {
            Ok(got) if got == row.given => {}
            Ok(got) => failures.push(format!(
                "{}: {:?} with {:?} was given as {got:?}, not {:?}",
                row.what, row.content, row.data, row.given
            )),
            Err(said) => failures.push(format!(
                "{}: {:?} with {:?} did not compile:\n{said}",
                row.what, row.content, row.data
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} rows:\n{}",
        failures.len(),
        ROWS.len(),
        failures.join("\n")
    );
}

/// The one join that cannot be made safe. Every plugin reads a dollar
/// directly before a reference as the escape for a literal, so a value that
/// ends in a dollar would turn the reference after it into text, and the
/// tool would never resolve it. That is refused, with the reference named,
/// and nothing is written.
#[test]
fn a_dollar_from_data_directly_before_a_reference_is_refused() {
    let row = Row {
        what: "a currency sign before a price",
        content: "costs ${{ data.currency }}${other.content}",
        variables: "",
        data: "currency: $\n",
        given: "",
    };
    let said = given(200, &row).expect_err("the reference would be lost");
    assert!(
        said.contains("FAILED")
            && said.contains("${other.content}")
            && said.contains("the reference would be lost")
            && said.contains("put a space or other text between the two"),
        "{said}"
    );

    // The same document with a value that ends in anything else compiles,
    // and the reference is still a reference.
    let fine = Row {
        data: "currency: EUR\n",
        ..row
    };
    assert_eq!(given(201, &fine).unwrap(), "costs EUR${other.content}");
}

/// The rule reduced to one sentence: after placement, the only references
/// in what a plugin is given are the ones the author wrote. Counted here by
/// reading what was given the way a plugin does.
#[test]
fn only_the_references_the_author_wrote_are_references() {
    use meridian_sdk::expr::{parse, Part};

    for (index, row) in ROWS.iter().enumerate() {
        let got = given(index + 100, row).unwrap_or_else(|said| panic!("{}: {said}", row.what));
        // What the author wrote, with every interpolation taken out: the
        // references left are the author's own.
        let mut authored = String::new();
        let mut rest = row.content;
        while let Some(start) = rest.find("${{") {
            authored.push_str(&rest[..start]);
            // A marker that cannot join what is on either side of it.
            authored.push(' ');
            let end = rest[start..].find("}}").expect("an interpolation closes");
            rest = &rest[start + end + 2..];
        }
        authored.push_str(rest);
        let references = |text: &str| -> Vec<String> {
            parse(text)
                .unwrap_or_else(|e| panic!("{}: {text:?} cannot be read: {e}", row.what))
                .into_iter()
                .filter_map(|part| match part {
                    Part::Expr(expr) => Some(expr),
                    Part::Text(_) => None,
                })
                .collect()
        };
        // A dollar the data supplied may stand before a brace the author
        // wrote; the author's text alone is then not a reference either.
        let expected = if authored.contains("${") {
            references(&authored)
        } else {
            Vec::new()
        };
        assert_eq!(references(&got), expected, "{}: given {got:?}", row.what);
    }
}
