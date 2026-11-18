//! The plugin settings reference is generated from what each runner
//! declares, and this test fails when the two differ. Record it again with
//! `UPDATE_GOLDEN=1` after a setting changes on purpose.
//!
//! It also holds every runner to the rules a setting must keep: a valid,
//! unique name, a variable that starts with `MERIDIAN_`, a description, and
//! a declaration for every `MERIDIAN_*` constant in its `runner.rs`, so a
//! setting that is read cannot go unlisted.

use std::collections::BTreeSet;
use std::fs;

use meridian_plugin_testkit::repo_root;
use meridian_sdk::Setting;

const PATH: &str = "docs/reference/plugin-settings.md";

const INTRODUCTION: &str = "\
# Plugin settings

Everything an operator can configure about how each target's runner drives
its tool, as each plugin itself declares it. This page is generated:
`crates/meridian-cli/tests/plugin_settings.rs` writes it from the plugins and
fails when it is out of date, so it cannot drift from the code.
`meridian plugins --settings` prints the same list for the plugins it finds.

A setting is read from the first of these that has it:

1. **The environment variable**, when it is set and not blank. It always wins,
   so a shell or a CI job can override what the project says.
2. **The project's `meridian.yaml`**, under `spec.settings.<target>.<name>`,
   using the short name in the first column. The settings travel with the
   project, and a name the plugin does not declare refuses the run before the
   plugin starts. See [the manifest](modules.md#plugin-settings).
3. **The default** in the third column; *none* means the setting is off or
   empty until set.

An argument list (`args`, `ssh-args`, `kubectl-args` and the like) is split
the way a POSIX shell splits a command line: words are separated by blanks,
`\"...\"` and `'...'` hold a word together, and a backslash outside single
quotes takes the next character as it is. So `--extra-vars \"a=b c\"` is two
arguments, the second holding a space, and a path with a space in it can be
quoted. A quote that is never closed takes the rest of the text.

A value that is a secret, such as a token, does not belong in a setting or in
`meridian.yaml`: declare it under `spec.secrets` in the document, and the
target hands it to its tool by a private channel. See
[Secrets](dsl/secrets.md#handing-a-secret-to-a-tool).

A setting that two targets share, such as `MERIDIAN_AZ_ARGS` for `arm` and
`bicep`, is one variable read by both plugins; under `spec.settings` it is set
for each target by name.

";

fn page() -> String {
    let mut out = INTRODUCTION.to_owned();
    for target in meridian_all_targets::ALL {
        let settings = target.runner().settings();
        out.push_str(&format!(
            "## [{name}](../targets/{name}.md)\n\n",
            name = target.name
        ));
        if settings.is_empty() {
            out.push_str("This target's runner declares no settings.\n\n");
            continue;
        }
        out.push_str("| Setting | Variable | Default | What it does |\n|---|---|---|---|\n");
        for s in &settings {
            let default = if s.default.is_empty() {
                "none".to_owned()
            } else {
                format!("`{}`", s.default)
            };
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} |\n",
                s.name,
                s.variable,
                default,
                s.description.replace('|', "\\|")
            ));
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_documented_settings_are_what_the_plugins_declare() {
    let path = repo_root().join(PATH);
    let want = page();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, &want).unwrap();
        return;
    }
    let have = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{PATH} is out of date; record it again with \
         `UPDATE_GOLDEN=1 cargo test -p meridian-cli --test plugin_settings`"
    );
}

#[test]
fn every_setting_is_well_formed_and_every_variable_is_declared() {
    let root = repo_root();
    let mut problems = Vec::new();
    for target in meridian_all_targets::ALL {
        let settings: Vec<Setting> = target.runner().settings();
        let mut names = BTreeSet::new();
        let mut variables = BTreeSet::new();
        for s in &settings {
            if !Setting::valid_name(&s.name) {
                problems.push(format!(
                    "{}: {:?} is not a setting name",
                    target.name, s.name
                ));
            }
            if !names.insert(s.name.clone()) {
                problems.push(format!(
                    "{}: setting {} is declared twice",
                    target.name, s.name
                ));
            }
            if !s.variable.starts_with("MERIDIAN_") {
                problems.push(format!(
                    "{}: setting {} reads {}, which does not start with MERIDIAN_",
                    target.name, s.name, s.variable
                ));
            }
            if !variables.insert(s.variable.clone()) {
                problems.push(format!(
                    "{}: {} is read by two settings",
                    target.name, s.variable
                ));
            }
            if s.description.trim().is_empty() {
                problems.push(format!(
                    "{}: setting {} has no description",
                    target.name, s.name
                ));
            }
        }
        // Every variable constant in the runner is declared: a setting the
        // runner reads but does not list would be invisible to operators.
        let runner = root.join("plugins").join(target.name).join("src/runner.rs");
        let source = fs::read_to_string(&runner).unwrap_or_default();
        for line in source.lines() {
            let Some(rest) = line.trim_start().strip_prefix("pub const ") else {
                continue;
            };
            let Some(start) = rest.find("\"MERIDIAN_") else {
                continue;
            };
            let variable = &rest[start + 1..];
            let Some(end) = variable.find('"') else {
                continue;
            };
            let variable = &variable[..end];
            if !variables.contains(variable) {
                problems.push(format!(
                    "{}: runner.rs reads {variable} but settings() does not declare it",
                    target.name
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
