//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{
    assert_golden, assert_golden_refusal, fixture, graph_from_file, graph_from_text, repo_root,
    without_positions,
};
use meridian_sdk::{
    Capabilities, Category, CompileError, Data, Emitted, Emitter, ResourceGraph, Value,
};
use meridian_target_cloud_init::{CloudInit, SMALLEST_CLOUD_LIMIT};

/// The shared fixtures, and whether this target compiles each or refuses it.
const FIXTURES: &[(&str, bool)] = &[
    ("accounts", false),
    ("restart_chain", true),
    ("runtime_condition", false),
    ("web_stack", true),
];

const EXAMPLE: &str = "examples/cloud-init/bootstrap.yaml";
const WEB_SERVER: &str = "examples/cloud-init/web-server.yaml";
const ALL_FEATURES: &str = "plugins/cloud-init/tests/all_features.yaml";

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    CloudInit.emit(graph, &Data::new())
}

/// A resource set with the given resources block, compiled.
fn compile(resources: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: cloud-init\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n{resources}"
    );
    emit(&graph_from_text("site.yaml", &text, "cloud-init"))
}

/// The user-data a resources block compiles to.
fn user_data(resources: &str) -> String {
    let emitted = compile(resources).unwrap_or_else(|e| panic!("compiling: {e}"));
    emitted.artifact.files["user-data"].clone()
}

fn refusal(resources: &str) -> CompileError {
    compile(resources).expect_err("the document should be refused")
}

/// The entries under one top-level key of some user-data, as written.
fn section<'a>(user_data: &'a str, key: &str) -> Vec<&'a str> {
    user_data
        .lines()
        .skip_while(|line| *line != format!("{key}:"))
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .collect()
}

/// User-data read back as YAML, the way cloud-init will read it.
fn parsed(user_data: &str) -> BTreeMap<String, Value> {
    match meridian_ir::parse_value("user-data", user_data.as_bytes()) {
        Ok(Value::Map(map)) => map,
        Ok(other) => panic!("user-data is {}, not a mapping", other.describe()),
        Err(e) => panic!("user-data is not YAML: {e}\n{user_data}"),
    }
}

// ---- golden files ---------------------------------------------------------

#[test]
fn every_shared_fixture_compiles_or_is_refused_as_recorded() {
    for (name, compiles) in FIXTURES {
        let graph = without_positions(fixture(name, "cloud-init"));
        match (emit(&graph), compiles) {
            (Ok(emitted), true) => assert_golden(&golden(), name, &emitted),
            (Err(refusal), false) => assert_golden_refusal(&golden(), name, &refusal),
            (Ok(_), false) => panic!("{name} compiled, and is recorded as refused"),
            (Err(e), true) => panic!("compiling {name}: {e}"),
        }
    }
}

#[test]
fn the_documented_example_compiles_to_the_bytes_recorded() {
    let emitted = emit(&graph_from_file(EXAMPLE, "cloud-init")).unwrap();
    assert_golden(&golden(), "bootstrap", &emitted);
}

#[test]
fn the_web_server_example_compiles_to_the_bytes_recorded() {
    let emitted = emit(&graph_from_file(WEB_SERVER, "cloud-init")).unwrap();
    assert_golden(&golden(), "web-server", &emitted);
}

/// One document that uses every key this target can write.
#[test]
fn every_feature_at_once_compiles_to_the_bytes_recorded() {
    let emitted = emit(&graph_from_file(ALL_FEATURES, "cloud-init")).unwrap();
    assert_golden(&golden(), "all_features", &emitted);
}

/// The page shows the example and what it compiles to. Both are checked
/// against the files, so the page cannot drift from what the plugin does.
#[test]
fn the_documentation_shows_the_example_and_its_output_as_they_are() {
    let root = repo_root();
    let page = fs::read_to_string(root.join("docs/targets/cloud-init.md")).unwrap();
    let source = fs::read_to_string(root.join(EXAMPLE)).unwrap();
    let document = &source[source.find("apiVersion:").unwrap()..];
    assert!(
        page.contains(&format!("```yaml\n{document}```")),
        "docs/targets/cloud-init.md does not show {EXAMPLE} as it is"
    );
    let output = fs::read_to_string(golden().join("bootstrap.user-data")).unwrap();
    assert!(
        page.contains(&format!("```yaml\n{output}```")),
        "docs/targets/cloud-init.md does not show the user-data the example compiles to"
    );
    let warnings = fs::read_to_string(golden().join("bootstrap.warnings")).unwrap();
    for warning in warnings.lines() {
        let text = warning.strip_prefix("cloud-init: ").unwrap();
        assert!(page.contains(text), "the page does not quote: {text}");
    }
}

#[test]
fn compiling_is_deterministic() {
    let graph = graph_from_file(EXAMPLE, "cloud-init");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

#[test]
fn every_recorded_user_data_is_a_cloud_config_document() {
    let known = ["groups", "packages", "runcmd", "users", "write_files"];
    let mut seen = 0;
    for entry in fs::read_dir(golden()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "user-data") {
            continue;
        }
        seen += 1;
        let text = fs::read_to_string(&path).unwrap();
        // cloud-init decides what user-data is by its first line alone.
        assert!(text.starts_with("#cloud-config\n"), "{}", path.display());
        // The native modules document writes keys of cloud-init's own, with
        // the values the document gave them.
        let native = path
            .file_name()
            .is_some_and(|n| n == "native_modules.user-data");
        for (key, value) in parsed(&text) {
            if known.contains(&key.as_str()) {
                assert!(matches!(value, Value::List(_)), "{key} is not a list");
            } else {
                assert!(native, "unexpected key {key}");
            }
        }
    }
    assert_eq!(
        seen, 6,
        "four documents of this plugin's and two shared fixtures"
    );
}

/// The commands of some user-data as the one script cloud-init makes of
/// them: each entry on a line of its own.
#[cfg(unix)]
fn script(user_data: &str) -> String {
    let doc = parsed(user_data);
    let mut script = String::new();
    for command in doc["runcmd"].as_list().unwrap() {
        script.push_str(command.as_str().expect("every command is a line of shell"));
        script.push('\n');
    }
    script
}

#[cfg(unix)]
fn sh(dir: &Path, args: &[&str], script: &str) -> std::process::Output {
    let file = dir.join("runcmd.sh");
    fs::write(&file, script).unwrap();
    std::process::Command::new("sh")
        .args(args)
        .arg(&file)
        .current_dir(dir)
        .output()
        .expect("sh runs")
}

#[cfg(unix)]
#[test]
fn every_recorded_command_list_is_a_script_sh_can_read() {
    let dir = meridian_plugin_testkit::scratch(env!("CARGO_TARGET_TMPDIR"), "syntax");
    for name in [
        "all_features",
        "bootstrap",
        "restart_chain",
        "web-server",
        "web_stack",
    ] {
        let text = fs::read_to_string(golden().join(format!("{name}.user-data"))).unwrap();
        let out = sh(&dir, &["-n"], &script(&text));
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The compiled commands, run for real in a scratch directory: the guards
/// guard, a directory change stays with its command, nothing is expanded
/// that did not ask for a shell, and a failure stops what follows it.
#[cfg(unix)]
#[test]
fn the_compiled_commands_do_what_the_document_says() {
    let dir = meridian_plugin_testkit::scratch(env!("CARGO_TARGET_TMPDIR"), "behaviour");
    let d = dir.display();
    fs::write(dir.join("old file"), "x").unwrap();
    let on = |id: &str| format!("      dependsOn:\n        - resource: {id}\n");
    let text = user_data(&format!(
        "    - id: sub\n      type: file\n      state: directory\n      params: {{path: \"{d}/sub dir\", mode: \"0700\"}}\n    \
         - id: inside\n      type: exec\n      params: {{command: touch made-inside, chdir: \"{d}/sub dir\"}}\n{}    \
         - id: outside\n      type: exec\n      params: {{command: touch made-outside}}\n{}    \
         - id: guarded\n      type: exec\n      params: {{command: touch guarded, creates: \"{d}/sub dir\"}}\n{}    \
         - id: unguarded\n      type: exec\n      params: {{command: touch unguarded, removes: \"{d}/sub dir\"}}\n{}    \
         - id: literal\n      type: exec\n      params: {{command: \"touch $((1+2)) *\"}}\n{}    \
         - id: expanded\n      type: exec\n      params:\n        shell: true\n        chdir: \"{d}/sub dir\"\n        command: \"echo $((1+2)) > sum # a comment\"\n{}    \
         - id: gone\n      type: file\n      state: absent\n      params: {{path: \"{d}/old file\"}}\n{}",
        on("sub"),
        on("inside"),
        on("outside"),
        on("guarded"),
        on("unguarded"),
        on("literal"),
        on("expanded"),
    ));
    let out = sh(&dir, &[], &script(&text));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let sub = dir.join("sub dir");
    assert!(sub.is_dir());
    assert!(sub.join("made-inside").exists());
    // The directory change did not outlive the command that asked for it.
    assert!(dir.join("made-outside").exists());
    assert!(!dir.join("guarded").exists(), "creates did not guard");
    assert!(
        dir.join("unguarded").exists(),
        "removes guarded what it should not"
    );
    assert!(dir.join("$((1+2))").exists() && dir.join("*").exists());
    assert_eq!(fs::read_to_string(sub.join("sum")).unwrap(), "3\n");
    assert!(!dir.join("old file").exists());

    let text = user_data(&format!(
        "    - id: fails\n      type: exec\n      params: {{command: \"false\"}}\n    \
         - id: after\n      type: exec\n      params: {{command: touch after-failure}}\n{}",
        on("fails")
    ));
    let out = sh(&dir, &[], &script(&text));
    assert!(!out.status.success());
    assert!(!dir.join("after-failure").exists());
}

// ---- what the plugin says it is --------------------------------------------

#[test]
fn the_target_declares_what_it_is_and_can_express() {
    assert_eq!(CloudInit.name(), "cloud-init");
    assert_eq!(CloudInit.category(), Category::ConfigurationManagement);
    assert_eq!(
        CloudInit.supported_resource_types(),
        [
            "<module>",
            "cron",
            "exec",
            "file",
            "group",
            "package",
            "packageRepository",
            "service",
            "user"
        ]
    );
    assert_eq!(
        CloudInit.capabilities(),
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
            secrets: false,
            credentials: false,
            cloud_credentials: false,
            adopt: meridian_sdk::Adoption::None,
        }
    );
}

// ---- the order of a first boot ----------------------------------------------

const PACKAGE: &str = "    - id: pkg\n      type: package\n      params: {name: nginx}\n";

#[test]
fn a_file_is_written_first_unless_it_has_something_to_wait_for() {
    let text = user_data(&format!(
        "{PACKAGE}    - id: early\n      type: file\n      params: {{path: /etc/early, content: a}}\n    \
         - id: late\n      type: file\n      params: {{path: /etc/late, content: b}}\n      dependsOn:\n        - resource: pkg\n"
    ));
    let doc = parsed(&text);
    let files = doc["write_files"].as_list().unwrap();
    let flags: Vec<(&str, bool)> = files
        .iter()
        .map(|f| {
            let f = f.as_map().unwrap();
            (f["path"].as_str().unwrap(), f.contains_key("defer"))
        })
        .collect();
    assert_eq!(flags, [("/etc/early", false), ("/etc/late", true)]);
}

#[test]
fn deferred_files_are_listed_after_the_rest_in_their_own_order() {
    let text = user_data(&format!(
        "{PACKAGE}    - id: late_a\n      type: file\n      params: {{path: /etc/late-a, content: a}}\n      dependsOn:\n        - resource: pkg\n    \
         - id: early\n      type: file\n      params: {{path: /etc/early, content: b}}\n    \
         - id: late_b\n      type: file\n      params: {{path: /etc/late-b, content: c}}\n      dependsOn:\n        - resource: late_a\n"
    ));
    let paths: Vec<&str> = section(&text, "write_files")
        .into_iter()
        .filter_map(|line| line.trim().strip_prefix("- path: "))
        .collect();
    assert_eq!(paths, ["/etc/early", "/etc/late-a", "/etc/late-b"]);
    assert_eq!(text.matches("defer: true").count(), 2, "{text}");
}

#[test]
fn a_file_owned_by_an_account_the_document_creates_waits_for_it() {
    let account = "    - id: grp\n      type: group\n      params: {name: ops}\n    \
                   - id: usr\n      type: user\n      params: {name: deploy}\n";
    for (owner, written) in [
        ("owner: deploy", "owner: deploy\n"),
        ("group: ops", "owner: root:ops\n"),
        ("owner: deploy, group: ops", "owner: deploy:ops\n"),
    ] {
        let text = user_data(&format!(
            "{account}    - id: conf\n      type: file\n      params: {{path: /etc/app, content: x, {owner}}}\n"
        ));
        assert!(text.contains(written), "{text}");
        assert!(text.contains("defer: true"), "{owner}: {text}");
    }
    // An owner the image already has is no reason to wait.
    let text = user_data(&format!(
        "{account}    - id: conf\n      type: file\n      params: {{path: /etc/app, content: x, owner: www-data}}\n"
    ));
    assert!(!text.contains("defer"), "{text}");
}

#[test]
fn a_dependency_cloud_init_cannot_honour_is_refused() {
    let exec = "    - id: setup\n      type: exec\n      params: {command: /bin/setup}\n";
    let on = |id: &str| format!("      dependsOn:\n        - resource: {id}\n");

    let err = refusal(&format!(
        "{exec}    - id: conf\n      type: file\n      params: {{path: /etc/app, content: x}}\n{}",
        on("setup")
    ));
    assert_eq!(err.rule, "unsatisfiable-order");
    assert_eq!(err.resource, "conf");
    assert_eq!(
        err.msg,
        "depends on \"setup\", but cloud-init writes files before it runs commands: \
         the order of a first boot is cloud-init's own and a document cannot change it"
    );
    assert_eq!((err.pos.file.as_str(), err.pos.line), ("site.yaml", 18));

    let err = refusal(&format!(
        "{PACKAGE}    - id: usr\n      type: user\n      params: {{name: deploy}}\n{}",
        on("pkg")
    ));
    assert!(
        err.msg
            .contains("cloud-init creates users before it installs packages"),
        "{}",
        err.msg
    );

    let err = refusal(&format!(
        "    - id: usr\n      type: user\n      params: {{name: deploy}}\n    \
         - id: grp\n      type: group\n      params: {{name: ops}}\n{}",
        on("usr")
    ));
    assert!(
        err.msg
            .contains("cloud-init creates groups before it creates users"),
        "{}",
        err.msg
    );

    // The file waits for the package, so a second package cannot wait for it.
    let err = refusal(&format!(
        "{PACKAGE}    - id: repo\n      type: file\n      params: {{path: /etc/repo, content: x}}\n{}    \
         - id: other\n      type: package\n      params: {{name: app}}\n{}",
        on("pkg"),
        on("repo")
    ));
    assert_eq!(err.resource, "other");
    assert!(
        err.msg.contains(
            "cloud-init installs packages before it writes the files that wait for users and packages"
        ),
        "{}",
        err.msg
    );
}

#[test]
fn dependencies_that_follow_the_boot_are_accepted_in_any_document_order() {
    // Listed back to front; every dependency still runs forwards.
    let text = user_data(
        "    - id: run\n      type: exec\n      params: {command: /bin/app}\n      dependsOn:\n        - resource: pkg\n        - resource: usr\n    \
         - id: pkg\n      type: package\n      params: {name: app}\n      dependsOn:\n        - resource: usr\n    \
         - id: usr\n      type: user\n      params: {name: deploy}\n      dependsOn:\n        - resource: grp\n    \
         - id: grp\n      type: group\n      params: {name: ops}\n",
    );
    let keys: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with([' ', '#']))
        .collect();
    assert_eq!(keys, ["groups:", "users:", "packages:", "runcmd:"]);
}

#[test]
fn commands_keep_the_order_the_dependencies_give_them() {
    let text = user_data(
        "    - id: second\n      type: exec\n      params: {command: /bin/second}\n      dependsOn:\n        - resource: first\n    \
         - id: first\n      type: exec\n      params: {command: /bin/first}\n",
    );
    assert_eq!(
        section(&text, "runcmd"),
        ["  - set -e", "  - /bin/first", "  - /bin/second"]
    );
}

#[test]
fn a_tree_whose_order_breaks_a_dependency_is_refused() {
    let mut graph = graph_from_text(
        "site.yaml",
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: cloud-init\n  targetHost:\n    from: inline\n    host: web01\n  resources:\n    \
         - id: first\n      type: exec\n      params: {command: /bin/first}\n    \
         - id: second\n      type: exec\n      params: {command: /bin/second}\n      dependsOn:\n        - resource: first\n",
        "cloud-init",
    );
    graph.resources.reverse();
    let err = emit(&graph).expect_err("the order contradicts the edge");
    assert_eq!(err.rule, "ordering-violated");
    assert_eq!(err.resource, "second");
}

// ---- users, groups and packages ---------------------------------------------

#[test]
fn adding_a_user_keeps_the_images_own_login_user() {
    let text = user_data("    - id: usr\n      type: user\n      params: {name: deploy}\n");
    assert_eq!(section(&text, "users"), ["  - default", "  - name: deploy"]);
    // A document with no users leaves cloud-init's default list alone.
    assert!(!user_data(PACKAGE).contains("users:"));
}

#[test]
fn a_user_carries_its_params_under_cloud_inits_names() {
    let text = user_data(
        "    - id: usr\n      type: user\n      params:\n        name: deploy\n        uid: 1200\n        home: /srv/deploy\n        \
         shell: /bin/bash\n        system: true\n        lock_passwd: false\n        primary_group: ops\n        groups: [sudo]\n        \
         gecos: Deploy\n        sudo: [\"ALL=(ALL) NOPASSWD:ALL\"]\n        ssh_authorized_keys:\n          - ssh-ed25519 AAAA one\n          - ssh-ed25519 BBBB two\n",
    );
    let doc = parsed(&text);
    let user = doc["users"].as_list().unwrap()[1].as_map().unwrap();
    let keys: Vec<&str> = user.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "gecos",
            "groups",
            "homedir",
            "lock_passwd",
            "name",
            "primary_group",
            "shell",
            "ssh_authorized_keys",
            "sudo",
            "system",
            "uid"
        ]
    );
    assert_eq!(user["homedir"], Value::String("/srv/deploy".into()));
    assert_eq!(user["uid"], Value::Int(1200));
    assert_eq!(user["system"], Value::Bool(true));
    assert_eq!(user["lock_passwd"], Value::Bool(false));
    // One group is still a list, and so is one sudo rule given as a list.
    assert_eq!(
        user["groups"],
        Value::List(vec![Value::String("sudo".into())])
    );
    assert_eq!(user["ssh_authorized_keys"].as_list().unwrap().len(), 2);
    assert!(
        text.contains("  - name: deploy\n    gecos: Deploy\n"),
        "{text}"
    );
}

#[test]
fn a_user_param_of_the_wrong_shape_is_refused() {
    let user = |param: &str| {
        refusal(&format!(
            "    - id: usr\n      type: user\n      params:\n        name: deploy\n        {param}\n"
        ))
    };
    let err = user("uid: \"1200\"");
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(
        err.msg,
        "param \"uid\" of a user must be a whole number, not a string"
    );
    let err = user("shell: true");
    assert_eq!(
        err.msg,
        "param \"shell\" of a user must be text, not a boolean"
    );
    let err = user("groups: [sudo, 4]");
    assert_eq!(
        err.msg,
        "param \"groups\" on resource type \"user\" must be a string or a list of strings, got a number"
    );
    let err = user("system: \"yes\"");
    assert_eq!(
        err.msg,
        "param \"system\" of a user must be true or false, not a string; write true or false \
         without quotes"
    );
    assert_eq!(user("password: hunter2").rule, "unsupported-param");
}

#[test]
fn what_a_first_boot_cannot_undo_is_refused_with_the_reason() {
    let err = refusal(
        "    - id: old\n      type: user\n      state: absent\n      params: {name: old}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert_eq!(
        err.msg,
        "resource type \"user\" does not accept state \"absent\" on this target (accepted: present): \
         cloud-init creates users on a first boot and cannot remove one; use an exec resource to \
         remove an account the image ships with"
    );
    for (kind, state) in [
        ("package", "absent"),
        ("package", "latest"),
        ("group", "absent"),
    ] {
        let err = refusal(&format!(
            "    - id: x\n      type: {kind}\n      state: {state}\n      params: {{name: x}}\n"
        ));
        assert_eq!(err.rule, "unsupported-state", "{kind} {state}");
        assert!(err.msg.contains("first boot"), "{}", err.msg);
    }
}

#[test]
fn a_group_is_its_name_or_its_name_over_its_members() {
    let text = user_data(
        "    - id: a\n      type: group\n      params: {name: ops}\n    \
         - id: b\n      type: group\n      params: {name: admins, members: [alice, bob]}\n",
    );
    assert_eq!(
        section(&text, "groups"),
        ["  - ops", "  - admins:", "      - alice", "      - bob"]
    );
    // cloud-init cannot choose a group's id, so asking for one is refused.
    let err = refusal("    - id: a\n      type: group\n      params: {name: ops, gid: 1200}\n");
    assert_eq!(
        err.msg,
        "resource type \"group\" does not accept param \"gid\" on this target (accepted: members, name)"
    );
}

#[test]
fn a_package_is_its_name_or_a_name_and_version_pair() {
    let text = user_data(
        "    - id: a\n      type: package\n      params: {name: nginx}\n    \
         - id: b\n      type: package\n      params: {name: \"libc6:i386\", version: \"2.39-0ubuntu8\"}\n",
    );
    assert_eq!(
        section(&text, "packages"),
        ["  - nginx", "  - - libc6:i386", "    - 2.39-0ubuntu8"]
    );
    let err =
        refusal("    - id: b\n      type: package\n      params: {name: nginx, version: 1.2}\n");
    assert_eq!(
        err.msg,
        "param \"version\" of a package must be text, not a number"
    );
}

#[test]
fn two_resources_for_one_thing_are_refused() {
    for (kind, rule) in [
        ("package", "duplicate-resource"),
        ("user", "duplicate-resource"),
        ("group", "duplicate-resource"),
    ] {
        let err = refusal(&format!(
            "    - id: a\n      type: {kind}\n      params: {{name: same}}\n    \
             - id: b\n      type: {kind}\n      params: {{name: same}}\n"
        ));
        assert_eq!(err.rule, rule);
        assert_eq!(err.resource, "b");
        assert!(err.msg.contains("resource \"a\""), "{}", err.msg);
    }
    let err = refusal("    - id: a\n      type: user\n      params: {name: \"de ploy\"}\n");
    assert_eq!(err.rule, "invalid-name");
}

// ---- files -------------------------------------------------------------------

#[test]
fn a_file_is_laid_out_in_a_fixed_key_order() {
    let text = user_data(
        "    - id: conf\n      type: file\n      params:\n        append: true\n        encoding: b64\n        mode: \"0600\"\n        \
         group: adm\n        owner: syslog\n        content: aGVsbG8K\n        path: /etc/app.conf\n",
    );
    assert_eq!(
        section(&text, "write_files"),
        [
            "  - path: /etc/app.conf",
            "    content: aGVsbG8K",
            "    owner: syslog:adm",
            "    permissions: \"0600\"",
            "    encoding: b64",
            "    append: true",
        ]
    );
}

#[test]
fn values_that_look_like_other_types_stay_strings() {
    let text = user_data(
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        mode: '0644'\n        owner: 'yes'\n        \
         content: \"a: b\\n# not a comment\\n\"\n",
    );
    assert!(text.contains("permissions: \"0644\""), "{text}");
    assert!(text.contains("owner: \"yes\""), "{text}");
    assert!(
        text.contains("content: |\n      a: b\n      # not a comment\n"),
        "{text}"
    );
    let doc = parsed(&text);
    let file = doc["write_files"].as_list().unwrap()[0].as_map().unwrap();
    assert_eq!(
        file["content"],
        Value::String("a: b\n# not a comment\n".into())
    );
    assert_eq!(file["permissions"], Value::String("0644".into()));
}

#[test]
fn a_directory_and_a_removal_are_commands() {
    let text = user_data(
        "    - id: dir\n      type: file\n      state: directory\n      params: {path: /srv/my app, owner: deploy, group: ops, mode: \"0750\"}\n    \
         - id: bare\n      type: file\n      state: directory\n      params: {path: /srv/bare}\n    \
         - id: gone\n      type: file\n      state: absent\n      params: {path: /etc/old.conf}\n",
    );
    assert_eq!(
        section(&text, "runcmd"),
        [
            "  - set -e",
            "  - install -d -m 0750 -o deploy -g ops '/srv/my app'",
            "  - install -d /srv/bare",
            "  - rm -rf -- /etc/old.conf",
        ]
    );
    assert!(!text.contains("write_files"), "{text}");
}

#[test]
fn file_params_that_contradict_the_state_are_refused() {
    let file = |state: &str, params: &str| {
        refusal(&format!(
            "    - id: f\n      type: file\n      state: {state}\n      params: {{{params}}}\n"
        ))
    };
    let err = file("directory", "path: /srv/app, content: x");
    assert_eq!(err.rule, "param-conflicts-with-state");
    assert_eq!(
        err.msg,
        "param \"content\" does not apply to a file in state \"directory\""
    );
    let err = file("absent", "path: /etc/old, mode: \"0644\"");
    assert_eq!(
        err.msg,
        "param \"mode\" does not apply to a file in state \"absent\""
    );
    // Without content cloud-init would write an empty file over the real one.
    let err = file("present", "path: /etc/app.conf, owner: root");
    assert_eq!(err.rule, "missing-param");
    assert!(
        err.msg.contains("it would empty /etc/app.conf"),
        "{}",
        err.msg
    );

    assert_eq!(file("present", "content: x").rule, "missing-param");
    assert_eq!(
        file("present", "path: etc/app, content: x").rule,
        "invalid-path"
    );
    assert_eq!(
        file("linked", "path: /etc/app, content: x").rule,
        "unsupported-state"
    );
    for root in ["/", "//", "/./"] {
        assert_eq!(
            file("absent", &format!("path: {root}")).rule,
            "invalid-path"
        );
    }
}

#[test]
fn a_mode_or_encoding_cloud_init_would_not_read_is_refused() {
    let file = |params: &str| {
        refusal(&format!(
            "    - id: f\n      type: file\n      params: {{path: /etc/app, content: x, {params}}}\n"
        ))
    };
    let err = file("mode: 644");
    assert_eq!(err.rule, "invalid-param");
    assert!(
        err.msg
            .ends_with("write it quoted, so it is not read as a number: \"644\""),
        "{}",
        err.msg
    );
    let err = file("mode: \"u+rw\"");
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(
        err.msg,
        "mode \"u+rw\" is not an octal file mode such as \"0644\""
    );
    let err = file("encoding: rot13");
    assert_eq!(err.rule, "invalid-param");
    assert!(
        err.msg.contains("accepted: b64, base64, gz,"),
        "{}",
        err.msg
    );
    assert_eq!(file("owner: \"root:adm\"").rule, "invalid-param");
    assert_eq!(file("append: \"yes\"").rule, "invalid-param");
}

#[test]
fn one_path_is_written_once_unless_the_second_appends() {
    let two = |extra: &str| {
        format!(
            "    - id: a\n      type: file\n      params: {{path: /etc/motd, content: one}}\n    \
             - id: b\n      type: file\n      params: {{path: /etc/motd, content: two{extra}}}\n"
        )
    };
    let err = refusal(&two(""));
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "b");
    assert_eq!(
        err.msg,
        "path \"/etc/motd\" is already written by resource \"a\"; set append: true if this one adds to it"
    );
    let text = user_data(&two(", append: true"));
    assert_eq!(text.matches("path: /etc/motd").count(), 2, "{text}");
}

// ---- services ----------------------------------------------------------------

#[test]
fn a_service_is_one_or_two_systemctl_commands() {
    let cases = [
        ("", "", vec!["systemctl start app"]),
        ("", "enabled: true", vec!["systemctl enable --now app"]),
        (
            "started",
            "enabled: true",
            vec!["systemctl enable --now app"],
        ),
        (
            "running",
            "enabled: false",
            vec!["systemctl disable app", "systemctl start app"],
        ),
        ("stopped", "", vec!["systemctl stop app"]),
        (
            "stopped",
            "enabled: false",
            vec!["systemctl disable --now app"],
        ),
        (
            "stopped",
            "enabled: true",
            vec!["systemctl enable app", "systemctl stop app"],
        ),
        ("restarted", "", vec!["systemctl restart app"]),
        (
            "reloaded",
            "enabled: true",
            vec!["systemctl enable app", "systemctl reload app"],
        ),
    ];
    for (state, enabled, want) in cases {
        let state_line = if state.is_empty() {
            String::new()
        } else {
            format!("      state: {state}\n")
        };
        let text = user_data(&format!(
            "    - id: svc\n      type: service\n{state_line}      params: {{name: app, {enabled}}}\n"
        ));
        let got: Vec<&str> = section(&text, "runcmd")
            .into_iter()
            .skip(1)
            .map(|line| line.trim_start_matches("  - "))
            .collect();
        assert_eq!(got, want, "state {state:?}, {enabled:?}");
    }
}

#[test]
fn services_are_named_in_one_warning() {
    let emitted = compile(
        "    - id: a\n      type: service\n      params: {name: nginx}\n    \
         - id: b\n      type: service\n      params: {name: redis}\n",
    )
    .unwrap();
    let warnings: Vec<String> = emitted.warnings.iter().map(|w| w.to_string()).collect();
    assert_eq!(
        warnings,
        ["cloud-init: cloud-init has no module for services, so resources \"a\", \"b\" compiled to \
          systemctl commands that run at the end of the first boot; the image must use systemd"]
    );
    assert!(compile(PACKAGE).unwrap().warnings.is_empty());

    let err = refusal(
        "    - id: a\n      type: service\n      state: paused\n      params: {name: nginx}\n",
    );
    assert_eq!(
        err.msg,
        "resource type \"service\" does not accept state \"paused\" on this target \
         (accepted: reloaded, restarted, running, started, stopped)"
    );
}

// ---- commands ----------------------------------------------------------------

fn exec(params: &str) -> String {
    let text = user_data(&format!(
        "    - id: run\n      type: exec\n      params:\n{params}"
    ));
    let doc = parsed(&text);
    let commands = doc["runcmd"].as_list().unwrap();
    assert_eq!(commands[0], Value::String("set -e".into()));
    assert_eq!(commands.len(), 2, "{text}");
    commands[1].as_str().unwrap().to_owned()
}

#[test]
fn a_command_without_a_shell_runs_its_words_and_expands_nothing() {
    assert_eq!(
        exec("        command: /usr/local/bin/warm-cache --all\n"),
        "/usr/local/bin/warm-cache --all"
    );
    assert_eq!(
        exec("        command: echo $HOME \"two words\" *.txt; reboot\n"),
        "echo '$HOME' 'two words' '*.txt;' reboot"
    );
    assert_eq!(
        exec("        command: echo \"it's\" ''\n        shell: false\n"),
        "echo 'it'\\''s' ''"
    );
}

#[test]
fn a_command_with_a_shell_is_the_authors_own_line() {
    assert_eq!(
        exec("        command: curl -fsS https://example.com/x | sh -s -- --yes\n        shell: true\n"),
        "curl -fsS https://example.com/x | sh -s -- --yes"
    );
}

#[test]
fn guards_wrap_a_command_without_a_shell_on_one_line() {
    assert_eq!(
        exec("        command: make install\n        chdir: /srv/my app\n"),
        "( cd '/srv/my app' && make install )"
    );
    assert_eq!(
        exec("        command: make install\n        creates: /usr/local/bin/app\n"),
        "if [ ! -e /usr/local/bin/app ]; then make install; fi"
    );
    assert_eq!(
        exec("        command: make install\n        removes: /srv/app/Makefile\n        creates: /usr/local/bin/app\n        chdir: /srv/app\n"),
        "if [ ! -e /usr/local/bin/app ] && [ -e /srv/app/Makefile ]; then ( cd /srv/app && make install ); fi"
    );
}

#[test]
fn guards_wrap_a_line_of_shell_without_letting_a_comment_swallow_them() {
    assert_eq!(
        exec("        command: \"make install # as root\\n\"\n        shell: true\n        chdir: /srv/app\n        creates: /usr/local/bin/app\n"),
        "if [ ! -e /usr/local/bin/app ]; then\n(\ncd /srv/app || exit\nmake install # as root\n)\nfi"
    );
}

#[test]
fn a_command_that_cannot_be_a_command_is_refused() {
    let run = |params: &str| refusal(&format!("    - id: run\n      type: exec\n{params}"));
    let err = run("      params: {command: \"echo 'unfinished\"}\n");
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(
        err.msg,
        "command \"echo 'unfinished\" cannot be split into words: a single quote is never closed. \
         Set shell: true if it is meant as a line of shell"
    );
    assert_eq!(
        run("      params: {command: \"   \"}\n").rule,
        "empty-param"
    );
    assert_eq!(run("      params: {chdir: /srv}\n").rule, "missing-param");
    assert_eq!(
        run("      params: {command: /bin/true, shell: \"yes\"}\n").rule,
        "invalid-param"
    );
    let err = run("      state: present\n      params: {command: /bin/true}\n");
    assert_eq!(
        err.msg,
        "resource type \"exec\" accepts no state on this target, got \"present\""
    );
}

#[test]
fn the_first_failure_stops_the_boots_commands() {
    // Present whenever there is a command, and never otherwise.
    let text = user_data("    - id: run\n      type: exec\n      params: {command: /bin/true}\n");
    assert_eq!(section(&text, "runcmd")[0], "  - set -e");
    assert!(!user_data(PACKAGE).contains("runcmd"));
}

// ---- notify ------------------------------------------------------------------

const NOTIFIERS: &str = "    - id: a\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: svc\n          action: restart\n    \
     - id: b\n      type: file\n      params: {path: /b, content: b}\n      notifies:\n        - resource: svc\n          action: restart\n        - resource: job\n          action: run\n    \
     - id: job\n      type: exec\n      params: {command: /bin/reindex, creates: /var/index}\n    \
     - id: svc\n      type: service\n      params: {name: app}\n    \
     - id: last\n      type: exec\n      params: {command: /bin/last}\n      dependsOn:\n        - resource: svc\n";

#[test]
fn a_notified_action_runs_once_after_every_other_command() {
    let text = user_data(NOTIFIERS);
    assert_eq!(
        section(&text, "runcmd"),
        [
            "  - set -e",
            "  - if [ ! -e /var/index ]; then /bin/reindex; fi",
            "  - systemctl start app",
            "  - /bin/last",
            // The two notifiers of the restart share it, and the handlers run
            // in name order: "restart svc" before "run job".
            "  - systemctl restart app",
            "  - if [ ! -e /var/index ]; then /bin/reindex; fi",
        ]
    );
}

#[test]
fn emulated_notify_is_announced() {
    let emitted = compile(NOTIFIERS).unwrap();
    let notices: Vec<&str> = emitted
        .warnings
        .iter()
        .map(|w| w.msg.as_str())
        .filter(|m| m.contains("notify is emulated"))
        .collect();
    assert_eq!(notices.len(), 1, "{:?}", emitted.warnings);
}

#[test]
fn a_notify_action_the_type_cannot_perform_is_refused() {
    let resources = "    - id: conf\n      type: file\n      params: {path: /etc/app.conf, content: x}\n      notifies:\n        - resource: svc\n          action: bounce\n    \
                     - id: svc\n      type: service\n      params: {name: app}\n";
    let err = refusal(resources);
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "svc");
    assert_eq!(
        err.msg,
        "resource type \"service\" does not support notify action \"bounce\" on this target \
         (supported: reload, restart, start, stop)"
    );
    let err = refusal(&resources.replace("type: service", "type: package"));
    assert_eq!(
        err.msg,
        "resource type \"package\" cannot be notified on this target, got action \"bounce\""
    );
    let err = refusal(
        &resources
            .replace("type: service", "type: exec")
            .replace("{name: app}", "{command: /bin/true}"),
    );
    assert!(err.msg.ends_with("(supported: run)"), "{}", err.msg);
}

// ---- the document as a whole ---------------------------------------------------

#[test]
fn an_unknown_type_names_the_types_that_work_and_the_line() {
    let err = refusal("    - id: fw\n      type: Firewall.Rule\n      params:\n        port: 80\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(err.resource, "fw");
    assert_eq!(err.target, "cloud-init");
    assert_eq!(
        err.msg,
        "unsupported resource type \"Firewall.Rule\" on this target: it is not one of the \
         shared types (cron, exec, file, group, package, packageRepository, service, user), and a cloud-config module \
         is written by its key, such as ntp or apt"
    );
    assert_eq!((err.pos.file.as_str(), err.pos.line), ("site.yaml", 11));
}

#[test]
fn a_runtime_condition_is_refused_not_compiled_away() {
    let err = refusal(
        "    - id: run\n      type: exec\n      runtimeWhen: \"ansible_facts['os_family'] == 'Debian'\"\n      params: {command: /bin/true}\n",
    );
    assert_eq!(err.rule, "unsupported-runtime-condition");
    assert_eq!(err.resource, "run");
}

#[test]
fn a_param_with_no_value_and_a_document_with_no_host_are_refused() {
    let mut graph = graph_from_file(EXAMPLE, "cloud-init");
    graph.host.clear();
    assert_eq!(emit(&graph).unwrap_err().rule, "missing-host");

    let mut graph = graph_from_file(EXAMPLE, "cloud-init");
    let first = &mut graph.resources[0];
    let key = first.params.keys().next().unwrap().clone();
    first.params.insert(key, Value::Null);
    assert_eq!(emit(&graph).unwrap_err().rule, "empty-param");
}

#[test]
fn the_meta_data_names_the_machine_unless_the_host_is_an_address() {
    let mut graph = graph_from_file(EXAMPLE, "cloud-init");
    let meta = |graph: &ResourceGraph| emit(graph).unwrap().artifact.files["meta-data"].clone();
    assert_eq!(
        meta(&graph),
        "instance-id: meridian-bootstrap\nlocal-hostname: app01.example.com\n"
    );
    for address in ["10.0.0.5", "2001:db8::5"] {
        graph.host = address.to_owned();
        assert_eq!(meta(&graph), "instance-id: meridian-bootstrap\n");
    }
}

#[test]
fn user_data_too_large_for_the_smallest_cloud_is_a_warning() {
    let body = "x".repeat(SMALLEST_CLOUD_LIMIT);
    let emitted = compile(&format!(
        "    - id: big\n      type: file\n      params: {{path: /etc/big, content: {body}}}\n"
    ))
    .unwrap();
    assert_eq!(emitted.warnings.len(), 1);
    let warning = emitted.warnings[0].to_string();
    assert!(
        warning.starts_with("cloud-init: the user-data is 16"),
        "{warning}"
    );
    assert!(warning.contains("Amazon EC2 accepts 16384"), "{warning}");
}

#[test]
fn a_reference_is_settled_from_the_document_or_refused() {
    let text = user_data(
        "    - id: root\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: page\n      type: file\n      params:\n        path: ${root.path}/index.html\n        content: \"export APP_HOME=$${HOME}/app\"\n",
    );
    assert!(text.contains("/srv/app/index.html"), "{text}");
    assert!(text.contains("export APP_HOME=${HOME}/app"), "{text}");
    assert!(!text.contains("${root"), "{text}");
    assert!(!text.contains("$${"), "{text}");

    let err = refusal(
        "    - id: page\n      type: file\n      params:\n        path: /etc/profile.d/app.sh\n        content: \"export APP_HOME=${HOME}/app\"\n",
    );
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "page");
    assert!(
        err.msg.contains("for a literal ${ write $${"),
        "{}",
        err.msg
    );
}

// ---- cloud-init's own modules beside the six -------------------------------

/// The document behind the `native_modules` golden files: cloud-config
/// modules beside the shared types, one a mapping, one a scalar, one a
/// list, and one notifying a service.
const NATIVE_MODULES: &str = "    - id: nginx_pkg\n      type: package\n      params:\n        name: nginx\n    - id: clock\n      type: ntp\n      params:\n        enabled: true\n        servers: [0.pool.ntp.org, 1.pool.ntp.org]\n    - id: zone\n      type: timezone\n      params:\n        value: Europe/Amsterdam\n    - id: early\n      type: bootcmd\n      params:\n        value: ['echo booting > /dev/kmsg']\n    - id: mirror\n      type: apt\n      params:\n        primary:\n          - arches: [default]\n            uri: http://deb.example.com/debian\n      notifies:\n        - resource: nginx_svc\n          action: restart\n    - id: nginx_svc\n      type: service\n      params:\n        name: nginx\n";

#[test]
fn a_module_is_written_under_its_key_with_its_value_as_written() {
    let emitted = compile(NATIVE_MODULES).unwrap_or_else(|e| panic!("{e}"));
    assert_golden(&golden(), "native_modules", &emitted);
    let text = &emitted.artifact.files["user-data"];
    assert!(
        text.contains(
            "ntp:\n  enabled: true\n  servers:\n    - 0.pool.ntp.org\n    - 1.pool.ntp.org\n"
        ),
        "{text}"
    );
    assert!(text.contains("timezone: Europe/Amsterdam\n"), "{text}");
    assert!(
        text.contains("bootcmd:\n  - echo booting > /dev/kmsg\n"),
        "{text}"
    );
    assert!(text.contains("apt:\n  primary:\n    - arches:\n        - default\n      uri: http://deb.example.com/debian\n"), "{text}");
    // A module may notify: the action runs once, last, as every notify does
    // here.
    assert!(text.contains("  - systemctl restart nginx\n"), "{text}");
    assert_eq!(parsed(text).len(), 6, "{text}");
}

#[test]
fn a_module_takes_its_whole_value_once_and_no_state() {
    let err = refusal("    - id: a\n      type: ntp\n      state: present\n      params:\n        enabled: true\n");
    assert_eq!(err.rule, "unsupported-state");

    let err = refusal("    - id: a\n      type: ntp\n      params:\n        value: {enabled: true}\n        servers: []\n");
    assert_eq!(err.rule, "invalid-param");

    let err = refusal("    - id: a\n      type: ntp\n      params: {}\n");
    assert_eq!(err.rule, "missing-param");

    let err = refusal("    - id: a\n      type: ntp\n      params:\n        servers: ~\n");
    assert_eq!(err.rule, "empty-param");

    let err = refusal(
        "    - id: a\n      type: ntp\n      params: {enabled: true}\n    - id: b\n      type: ntp\n      params: {enabled: false}\n",
    );
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "b");
}

#[test]
fn a_name_that_is_not_a_module_or_that_a_shared_type_writes_is_refused() {
    for r#type in [
        "Ntp",
        "ansible.builtin.cron",
        "mysql::db",
        "native/cron",
        "native/ntp",
    ] {
        let err = refusal(&format!(
            "    - id: a\n      type: '{type}'\n      params:\n        value: 1\n"
        ));
        assert_eq!(err.rule, "unsupported-resource-type", "{type}");
    }
    // The six have no module of their own name, and the keys they write
    // are theirs alone.
    let err = refusal("    - id: a\n      type: native/file\n      params:\n        path: /x\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert!(err.msg.contains("write_files"), "{}", err.msg);
    let err = refusal("    - id: a\n      type: write_files\n      params:\n        value: []\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(
        err.msg,
        "resource type \"write_files\" is the key the shared type file writes; write a file \
         resource, so that the two never write one key"
    );
}

#[test]
fn nothing_can_be_ordered_around_a_module_and_a_module_cannot_be_notified() {
    let err = refusal(
        "    - id: a\n      type: ntp\n      params: {enabled: true}\n    - id: b\n      type: package\n      params: {name: nginx}\n      dependsOn:\n        - resource: a\n",
    );
    assert_eq!(err.rule, "unsatisfiable-order");
    assert_eq!(err.resource, "b");
    assert!(err.msg.contains("cloud-config module"), "{}", err.msg);

    let err = refusal(
        "    - id: a\n      type: ntp\n      params: {enabled: true}\n      dependsOn:\n        - resource: b\n    - id: b\n      type: package\n      params: {name: nginx}\n",
    );
    assert_eq!(err.rule, "unsatisfiable-order");

    let err = refusal(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: a\n          action: run\n    - id: a\n      type: ntp\n      params: {enabled: true}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "a");
}

#[test]
fn the_schemas_offer_any_module_as_an_open_pattern() {
    let schemas = CloudInit.resource_schemas();
    let pattern = schemas
        .iter()
        .find(|s| s.r#type == meridian_target_cloud_init::NATIVE_PATTERN)
        .expect("the module pattern is declared");
    assert!(pattern.open && pattern.is_pattern() && pattern.params.is_empty());
    assert!(pattern.actions.is_empty());
}

const APT_REPO: &str = "    - id: repo\n      type: packageRepository\n      params:\n        name: nginx\n        format: apt\n        url: https://nginx.org/packages/debian\n        distribution: bookworm\n";

#[test]
fn repositories_are_apt_sources_and_yum_repos_added_before_packages() {
    let text = user_data(&format!(
        "{APT_REPO}    - id: el\n      type: packageRepository\n      params:\n        name: nginx-el\n        format: yum\n        url: https://nginx.org/packages/rhel/9/x86_64/\n        key: https://nginx.org/keys/nginx_signing.key\n    - id: pkg\n      type: package\n      dependsOn:\n        - resource: repo\n        - resource: el\n      params:\n        name: nginx\n"
    ));
    let config = parsed(&text);
    assert_eq!(
        config["apt"],
        Value::Map(BTreeMap::from([(
            "sources".to_owned(),
            Value::Map(BTreeMap::from([(
                "nginx.list".to_owned(),
                Value::Map(BTreeMap::from([(
                    "source".to_owned(),
                    Value::String("deb https://nginx.org/packages/debian bookworm main".to_owned())
                )]))
            )]))
        )]))
    );
    let yum = section(&text, "yum_repos").join("\n");
    for line in [
        "baseurl: https://nginx.org/packages/rhel/9/x86_64/",
        "gpgcheck: true",
        "gpgkey: https://nginx.org/keys/nginx_signing.key",
    ] {
        assert!(yum.contains(line), "{line} in {text}");
    }
}

#[test]
fn a_repository_cloud_init_cannot_write_is_refused() {
    let err = refusal(&APT_REPO.replace("      params:", "      state: absent\n      params:"));
    assert_eq!(err.rule, "unsupported-state");
    let err = refusal(&format!(
        "{APT_REPO}        key: https://nginx.org/keys/nginx_signing.key\n"
    ));
    assert_eq!(err.rule, "unsupported-param");
    let err = refusal(&format!(
        "{APT_REPO}    - id: proxy\n      type: apt\n      params:\n        http_proxy: http://proxy:3128\n"
    ));
    assert_eq!(err.rule, "duplicate-resource");
}

#[test]
fn a_cron_job_is_a_file_in_cron_d() {
    let text = user_data("    - id: rotate\n      type: cron\n      params:\n        name: logrotate\n        command: /usr/sbin/logrotate /etc/logrotate.conf\n        minute: 0\n        hour: 2\n");
    assert!(text.contains("/etc/cron.d/logrotate"), "{text}");
    assert!(
        text.contains("0 2 * * * root /usr/sbin/logrotate /etc/logrotate.conf"),
        "{text}"
    );
}

#[test]
fn a_download_is_refused() {
    let err = refusal("    - id: tool\n      type: file\n      params:\n        path: /usr/local/bin/tool\n        source: https://example.com/tool\n        checksum: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n");
    assert_eq!(err.rule, "unsupported-param", "{}", err.msg);
}
