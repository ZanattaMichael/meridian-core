//! The compile half, without a process boundary.
//!
//! The golden files under `testdata` are what the shared fixtures compile to.
//! `tests/real_salt.rs` hands the same files to Salt itself, so what is
//! recorded here is known to render, compile and resolve its requisites.

#![allow(clippy::result_large_err)]

use std::path::Path;

use meridian_plugin_testkit::{
    assert_golden, fixture, graph_from_file, graph_from_text, repo_root, secret_in_param,
    without_positions,
};
use meridian_sdk::{
    Capabilities, Category, CompileError, Data, Edge, EdgeKind, Emitted, Emitter, Position,
    ResourceGraph, Value,
};
use meridian_target_salt::Salt;

const FIXTURES: &[&str] = &[
    "accounts",
    "restart_chain",
    "runtime_condition",
    "web_stack",
];

const SLS: &str = "states/web.sls";
const EXAMPLE: &str = "examples/salt/web-server.yaml";

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Salt.emit(graph, &Data::new())
}

fn document(resources: &str) -> String {
    format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: salt\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n{resources}"
    )
}

/// A resource set with the given resources block, compiled.
fn compile(resources: &str) -> Result<Emitted, CompileError> {
    emit(&graph_from_text("site.yaml", &document(resources), "salt"))
}

fn sls(resources: &str) -> String {
    let emitted = compile(resources).unwrap_or_else(|e| panic!("compiling: {e}"));
    emitted.artifact.files[SLS].clone()
}

fn refusal(resources: &str) -> CompileError {
    compile(resources).expect_err("the document should be refused")
}

/// The runtime-condition fixture is written in Ansible's terms, as the
/// placeholder target it names. For this target the condition is a Jinja
/// expression over the minion's grains.
fn in_salt_terms(mut graph: ResourceGraph) -> ResourceGraph {
    for resource in &mut graph.resources {
        if !resource.runtime_when.is_empty() {
            resource.runtime_when = "grains['os_family'] == 'Debian'".to_owned();
        }
    }
    graph
}

#[test]
fn every_fixture_compiles_to_its_golden_files() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    for name in FIXTURES {
        let graph = in_salt_terms(without_positions(fixture(name, "salt")));
        let emitted = emit(&graph).unwrap_or_else(|e| panic!("compiling {name}: {e}"));
        assert_golden(&golden, name, &emitted);
    }
}

/// The document the user guide walks through. Its golden files are what the
/// guide shows, so the guide cannot drift from what the plugin writes.
#[test]
fn the_guides_example_compiles_to_what_the_guide_shows() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let graph = without_positions(graph_from_file(EXAMPLE, "salt"));
    let emitted = emit(&graph).unwrap_or_else(|e| panic!("compiling the example: {e}"));
    assert!(emitted.warnings.is_empty(), "{:?}", emitted.warnings);
    assert_golden(&golden, "example", &emitted);

    let guide = std::fs::read_to_string(repo_root().join("docs/targets/salt.md")).unwrap();
    for text in emitted.artifact.files.values() {
        assert!(
            guide.contains(text.as_str()),
            "docs/targets/salt.md does not show the compiled example:\n{text}"
        );
    }
}

/// Every rule the plugin can refuse with is in the guide, so a refusal a
/// person meets is one they can look up.
#[test]
fn the_guide_documents_every_refusal_rule() {
    let root = repo_root();
    let guide = std::fs::read_to_string(root.join("docs/targets/salt.md")).unwrap();
    let mut rules = std::collections::BTreeSet::new();
    for file in ["transform.rs", "sort.rs", "validate.rs"] {
        let source = std::fs::read_to_string(root.join("plugins/salt/src").join(file)).unwrap();
        // A rule is a constant of the registry (`rules::UNSUPPORTED_PARAM`)
        // or of this plugin's own table (`own::SHELL_REQUIRED`); its name is
        // the constant's, in lower case with hyphens.
        for line in source.lines() {
            for prefix in ["rules::", "own::"] {
                for (at, _) in line.match_indices(prefix) {
                    let rest = &line[at + prefix.len()..];
                    let end = rest
                        .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                        .unwrap_or(rest.len());
                    let word = &rest[..end];
                    if word.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                        rules.insert(word.to_ascii_lowercase().replace('_', "-"));
                    }
                }
            }
        }
    }
    assert!(rules.len() >= 15, "only found {rules:?}");
    for rule in &rules {
        assert!(
            guide.contains(&format!("`{rule}`")),
            "docs/targets/salt.md does not document the rule {rule}"
        );
    }
}

#[test]
fn compiling_is_deterministic() {
    let graph = fixture("web_stack", "salt");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

#[test]
fn the_plugin_says_what_it_is() {
    assert_eq!(Salt.name(), "salt");
    assert_eq!(Salt.category(), Category::ConfigurationManagement);
    assert_eq!(
        Salt.capabilities(),
        Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: true,
            secrets: true,
            credentials: true,
            cloud_credentials: false,
            adopt: meridian_sdk::Adoption::Converge,
        }
    );
    assert_eq!(
        Salt.supported_resource_types(),
        [
            "<module>.<function>",
            "exec",
            "file",
            "group",
            "package",
            "packageRepository",
            "service",
            "user"
        ]
    );
}

#[test]
fn the_top_file_assigns_the_state_to_exactly_its_host() {
    let emitted =
        compile("    - id: p\n      type: package\n      params: {name: nginx}\n").unwrap();
    assert_eq!(
        emitted.artifact.files.keys().collect::<Vec<_>>(),
        ["states/top.sls", "states/web.sls"]
    );
    assert_eq!(
        emitted.artifact.files["states/top.sls"],
        "#!yaml\n# Generated by Meridian. Do not edit; this file is compiled from a Meridian document.\n\n\
         base:\n  web01.example.com:\n    - match: list\n    - web\n"
    );
}

// The mapping, one resource type at a time.

#[test]
fn a_package_is_installed_latest_or_removed() {
    let text = sls(
        "    - id: a\n      type: package\n      params: {name: nginx, version: 1.24.0-1}\n    - id: b\n      type: package\n      state: latest\n      params: {name: curl}\n    - id: c\n      type: package\n      state: absent\n      params: {name: telnet}\n",
    );
    assert!(
        text.contains("a:\n  pkg.installed:\n    - name: nginx\n    - version: 1.24.0-1\n"),
        "{text}"
    );
    assert!(
        text.contains("b:\n  pkg.latest:\n    - name: curl\n"),
        "{text}"
    );
    assert!(
        text.contains("c:\n  pkg.removed:\n    - name: telnet\n"),
        "{text}"
    );
}

#[test]
fn a_version_cannot_be_pinned_on_a_package_that_is_latest_or_absent() {
    let err = refusal(
        "    - id: b\n      type: package\n      state: latest\n      params: {name: curl, version: '8'}\n",
    );
    assert_eq!(err.rule, "param-conflicts-with-state");
    assert_eq!(
        err.msg,
        "param \"version\" cannot be used with state \"latest\", which compiles to pkg.latest \
         (it is accepted with state: present)"
    );
}

#[test]
fn a_service_runs_or_is_dead_and_enabled_becomes_enable() {
    let text = sls(
        "    - id: up\n      type: service\n      params: {name: nginx, enabled: true}\n    - id: down\n      type: service\n      state: stopped\n      params: {name: telnetd, enabled: false}\n",
    );
    assert!(
        text.contains("up:\n  service.running:\n    - name: nginx\n    - enable: true\n"),
        "{text}"
    );
    assert!(
        text.contains("down:\n  service.dead:\n    - name: telnetd\n    - enable: false\n"),
        "{text}"
    );
}

#[test]
fn a_file_with_content_is_managed_and_one_without_keeps_what_it_holds() {
    let text = sls(
        "    - id: conf\n      type: file\n      params: {path: /etc/app.conf, content: \"a = 1\\n\", owner: root, group: wheel, mode: '0640'}\n    - id: touch\n      type: file\n      state: file\n      params: {path: /var/log/app.log, owner: app}\n",
    );
    assert!(
        text.contains(
            "conf:\n  file.managed:\n    - name: /etc/app.conf\n    - contents: |\n        a = 1\n    - group: wheel\n    - mode: \"0640\"\n    - user: root\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "touch:\n  file.managed:\n    - name: /var/log/app.log\n    - replace: false\n    - user: app\n"
        ),
        "{text}"
    );
}

#[test]
fn a_directory_is_made_with_its_parents() {
    let text = sls(
        "    - id: d\n      type: file\n      state: directory\n      params: {path: /srv/app/data, mode: '0750'}\n",
    );
    assert!(
        text.contains(
            "d:\n  file.directory:\n    - name: /srv/app/data\n    - makedirs: true\n    - mode: \"0750\"\n"
        ),
        "{text}"
    );
}

#[test]
fn literal_content_cannot_be_a_directory_or_absent() {
    for state in ["directory", "absent"] {
        let err = refusal(&format!(
            "    - id: conf\n      type: file\n      state: {state}\n      params:\n        path: /etc/app\n        content: hello\n"
        ));
        assert_eq!(err.rule, "param-conflicts-with-state");
        assert_eq!(
            err.msg,
            format!(
                "param \"content\" cannot be used with state \"{state}\", which compiles to file.{state} \
                 (it is accepted with state: file, present)"
            )
        );
    }
}

#[test]
fn what_describes_an_absent_thing_is_left_out_with_a_warning() {
    let emitted = compile(
        "    - id: old\n      type: user\n      state: absent\n      params: {name: old, uid: 1500, shell: /bin/sh}\n    - id: gone\n      type: file\n      state: absent\n      params: {path: /etc/old.conf, mode: '0644'}\n    - id: grp\n      type: group\n      state: absent\n      params: {name: old, gid: 1500}\n",
    )
    .unwrap();
    let text = &emitted.artifact.files[SLS];
    assert!(
        text.contains("old:\n  user.absent:\n    - name: old\n\n"),
        "{text}"
    );
    assert!(
        text.contains("gone:\n  file.absent:\n    - name: /etc/old.conf\n\n"),
        "{text}"
    );
    assert!(
        text.ends_with("grp:\n  group.absent:\n    - name: old\n"),
        "{text}"
    );
    let warnings: Vec<String> = emitted.warnings.iter().map(|w| w.to_string()).collect();
    assert_eq!(
        warnings,
        [
            "salt: old: param \"shell\" means nothing for state \"absent\" and was left out",
            "salt: old: param \"uid\" means nothing for state \"absent\" and was left out",
            "salt: gone: param \"mode\" means nothing for state \"absent\" and was left out",
            "salt: grp: param \"gid\" means nothing for state \"absent\" and was left out",
        ]
    );
}

#[test]
fn a_user_and_a_group_keep_their_params() {
    let text = sls(
        "    - id: g\n      type: group\n      params: {name: deploy, gid: 1200, system: true}\n    - id: u\n      type: user\n      params: {name: deploy, uid: 1200, home: /srv/deploy, shell: /bin/bash, system: false, groups: [deploy, sudo]}\n",
    );
    assert!(
        text.contains(
            "g:\n  group.present:\n    - name: deploy\n    - gid: 1200\n    - system: true\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "u:\n  user.present:\n    - name: deploy\n    - groups:\n        - deploy\n        - sudo\n    - home: /srv/deploy\n    - shell: /bin/bash\n    - system: false\n    - uid: 1200\n"
        ),
        "{text}"
    );
}

#[test]
fn an_exec_is_a_command_with_its_guards() {
    let text = sls(
        "    - id: run\n      type: exec\n      params: {command: /usr/bin/make install, chdir: /srv/src, creates: /usr/local/bin/app, removes: /srv/src/Makefile}\n",
    );
    assert!(
        text.contains(
            "run:\n  cmd.run:\n    - name: /usr/bin/make install\n    - creates: /usr/local/bin/app\n    - cwd: /srv/src\n    - onlyif:\n        - fun: file.access\n          mode: f\n          path: /srv/src/Makefile\n"
        ),
        "{text}"
    );
}

#[test]
fn a_command_a_shell_would_reinterpret_must_ask_for_a_shell() {
    for command in [
        "a | b",
        "echo $HOME",
        "rm /tmp/*.log",
        "a && b",
        "echo hi > /tmp/x",
    ] {
        let err = refusal(&format!(
            "    - id: run\n      type: exec\n      params:\n        command: '{command}'\n"
        ));
        assert_eq!(err.rule, "shell-required", "{command}");
        assert!(
            err.msg.contains("set param \"shell\" to true"),
            "{}",
            err.msg
        );
    }
    let text =
        sls("    - id: run\n      type: exec\n      params: {command: 'a | b', shell: true}\n");
    assert!(
        text.contains("run:\n  cmd.run:\n    - name: a | b\n"),
        "{text}"
    );
    assert!(!text.contains("shell"), "{text}");
    // Quoting reads the same with a shell as without one.
    sls("    - id: run\n      type: exec\n      params: {command: \"logger 'it works'\"}\n");
}

// Refusals.

#[test]
fn an_unknown_type_names_the_types_that_work_and_the_line() {
    let err = refusal("    - id: fw\n      type: firewall_rule\n      params:\n        port: 80\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(err.resource, "fw");
    assert_eq!(err.target, "salt");
    assert_eq!(
        err.msg,
        "unsupported resource type \"firewall_rule\" on this target: it is not one of the \
         shared types (exec, file, group, package, packageRepository, service, user), and a Salt state is \
         written by its dotted name, such as pkgrepo.managed"
    );
    assert_eq!((err.pos.file.as_str(), err.pos.line), ("site.yaml", 11));
}

#[test]
fn a_param_the_type_does_not_take_is_refused_not_dropped() {
    let err = refusal(
        "    - id: nginx\n      type: package\n      params:\n        name: nginx\n        repo: main\n",
    );
    assert_eq!(err.rule, "unsupported-param");
    assert_eq!(
        err.msg,
        "resource type \"package\" does not accept param \"repo\" on this target (accepted: name, version)"
    );
}

#[test]
fn a_missing_required_param_is_refused() {
    let err = refusal("    - id: conf\n      type: file\n      params:\n        owner: root\n");
    assert_eq!(err.rule, "missing-param");
    assert_eq!(
        err.msg,
        "resource type \"file\" requires param \"path\" on this target"
    );
}

#[test]
fn a_state_outside_the_vocabulary_names_the_ones_accepted() {
    let err = refusal(
        "    - id: nginx\n      type: service\n      state: restarted\n      params:\n        name: nginx\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert_eq!(
        err.msg,
        "resource type \"service\" does not accept state \"restarted\" on this target \
         (accepted: running, started, stopped)"
    );
}

#[test]
fn exec_takes_no_state() {
    let err = refusal(
        "    - id: run\n      type: exec\n      state: present\n      params:\n        command: /bin/true\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert_eq!(
        err.msg,
        "resource type \"exec\" accepts no state on this target, got \"present\""
    );
}

#[test]
fn a_param_with_no_value_is_refused() {
    let mut graph = fixture("accounts", "salt");
    let first = &mut graph.resources[0];
    let key = first.params.keys().next().unwrap().clone();
    first.params.insert(key, Value::Null);
    first.pos = Position::default();
    assert_eq!(emit(&graph).unwrap_err().rule, "empty-param");
}

// Notifications.

const CONF: &str = "    - id: conf\n      type: file\n      params: {path: /etc/app.conf, content: x}\n      notifies:\n        - resource: svc\n          action: ACTION\n";
const SVC: &str = "    - id: svc\n      type: service\n      params: {name: app}\n";

#[test]
fn a_restart_is_a_watch_on_the_service_itself() {
    let text = sls(&format!("{}{SVC}", CONF.replace("ACTION", "restart")));
    assert!(
        text.ends_with(
            "svc:\n  service.running:\n    - name: app\n    - watch:\n        - file: conf\n"
        ),
        "{text}"
    );
}

#[test]
fn a_watch_is_not_repeated_as_a_require() {
    let conf = CONF.replace("ACTION", "restart");
    let svc = format!("{SVC}      dependsOn:\n        - resource: conf\n");
    let text = sls(&format!("{conf}{svc}"));
    assert!(
        text.contains("    - watch:\n        - file: conf\n"),
        "{text}"
    );
    assert!(!text.contains("require"), "{text}");
}

#[test]
fn a_restart_from_a_resource_applied_later_gets_a_state_after_both() {
    // The file depends on the service, so it is applied second; a watch on
    // the service's own state would turn that order round.
    let conf = format!(
        "{}      dependsOn:\n        - resource: svc\n",
        CONF.replace("ACTION", "restart")
    );
    let text = sls(&format!("{SVC}{conf}"));
    assert!(
        text.contains("svc:\n  service.running:\n    - name: app\n\nconf:"),
        "{text}"
    );
    assert!(
        text.ends_with(
            "restart svc:\n  service.running:\n    - name: app\n    - require:\n        - service: svc\n    - watch:\n        - file: conf\n"
        ),
        "{text}"
    );
}

#[test]
fn a_reload_is_a_second_state_that_watches() {
    let text = sls(&format!("{}{SVC}", CONF.replace("ACTION", "reload")));
    assert!(
        text.ends_with(
            "reload svc:\n  service.running:\n    - name: app\n    - reload: true\n    - require:\n        - service: svc\n    - watch:\n        - file: conf\n"
        ),
        "{text}"
    );
}

#[test]
fn start_stop_and_run_happen_only_on_a_change() {
    let text = sls(&format!("{}{SVC}", CONF.replace("ACTION", "stop")));
    assert!(
        text.ends_with(
            "stop svc:\n  service.dead:\n    - name: app\n    - require:\n        - service: svc\n    - onchanges:\n        - file: conf\n"
        ),
        "{text}"
    );
    let text = sls(&format!("{}{SVC}", CONF.replace("ACTION", "start")));
    assert!(
        text.ends_with(
            "start svc:\n  service.running:\n    - name: app\n    - require:\n        - service: svc\n    - onchanges:\n        - file: conf\n"
        ),
        "{text}"
    );
    let job = "    - id: job\n      type: exec\n      params: {command: /bin/reindex, creates: /var/idx}\n";
    let text = sls(&format!(
        "{}{job}",
        CONF.replace("ACTION", "run").replace("svc", "job")
    ));
    assert!(
        text.ends_with(
            "run job:\n  cmd.run:\n    - name: /bin/reindex\n    - creates: /var/idx\n    - require:\n        - cmd: job\n    - onchanges:\n        - file: conf\n"
        ),
        "{text}"
    );
}

#[test]
fn two_notifiers_of_one_change_share_a_state() {
    let text = sls(
        "    - id: a\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: svc\n          action: reload\n    - id: b\n      type: file\n      params: {path: /b, content: b}\n      notifies:\n        - resource: svc\n          action: reload\n    - id: svc\n      type: service\n      params: {name: app}\n",
    );
    assert_eq!(text.matches("reload svc:").count(), 1, "{text}");
    assert!(
        text.ends_with("    - watch:\n        - file: a\n        - file: b\n"),
        "{text}"
    );
}

#[test]
fn a_stopped_service_cannot_be_restarted_or_reloaded() {
    let svc =
        "    - id: svc\n      type: service\n      state: stopped\n      params: {name: app}\n";
    for action in ["restart", "reload"] {
        let err = refusal(&format!("{}{svc}", CONF.replace("ACTION", action)));
        assert_eq!(err.rule, "unsupported-action");
        assert_eq!(err.resource, "svc");
        assert_eq!(
            err.msg,
            format!(
                "cannot be notified with action \"{action}\" while its state is \"stopped\": \
                 Salt restarts or reloads a service through the state that keeps it running, \
                 and this one is to be stopped"
            )
        );
    }
}

#[test]
fn a_notify_action_the_type_cannot_perform_is_refused() {
    let resources = format!("{}{SVC}", CONF.replace("ACTION", "bounce"));
    let err = refusal(&resources);
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
}

// Runtime conditions and what Jinja must not touch.

#[test]
fn a_file_with_no_runtime_condition_is_plain_yaml() {
    let text = sls(
        "    - id: tpl\n      type: file\n      params:\n        path: /etc/alerts.yml\n        content: \"summary: {{ $labels.instance }} {% raw %} {# x #}\\n\"\n",
    );
    assert!(text.starts_with("#!yaml\n"), "{text}");
    assert!(
        text.contains(
            "    - contents: |\n        summary: {{ $labels.instance }} {% raw %} {# x #}\n"
        ),
        "{text}"
    );
}

#[test]
fn a_runtime_condition_is_a_jinja_test_that_keeps_the_state_in_the_file() {
    let text = sls(
        "    - id: tune\n      type: exec\n      runtimeWhen: grains['os_family'] == 'Debian'\n      params: {command: /usr/sbin/tune}\n",
    );
    assert_eq!(
        text,
        "#!jinja|yaml\n# Generated by Meridian. Do not edit; this file is compiled from a Meridian document.\n\
         {%- set meridian_brace = \"{\" %}\n\n\
         tune:\n  cmd.run:\n    - name: /usr/sbin/tune\n\
         {%- if not (grains['os_family'] == 'Debian') %}\n    - unless: exit 0\n{%- endif %}\n"
    );
}

#[test]
fn text_that_looks_like_a_template_survives_a_rendered_file() {
    let text = sls(
        "    - id: tune\n      type: exec\n      runtimeWhen: grains['kernel'] == 'Linux'\n      params: {command: /usr/sbin/tune}\n    - id: tpl\n      type: file\n      params:\n        path: /etc/alerts.yml\n        content: \"summary: {{ $labels.instance }}\\n\"\n",
    );
    assert!(
        text.contains(
            "    - contents: |\n        summary: {{meridian_brace}}{ $labels.instance }}\n"
        ),
        "{text}"
    );
}

#[test]
fn a_reaction_does_not_inherit_a_runtime_condition() {
    let text = sls(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: job\n          action: run\n    - id: job\n      type: exec\n      runtimeWhen: grains['os_family'] == 'Debian'\n      params: {command: /bin/reindex}\n",
    );
    assert_eq!(text.matches("{%- if not").count(), 1, "{text}");
    assert!(
        text.ends_with("    - onchanges:\n        - file: conf\n"),
        "{text}"
    );
}

#[test]
fn a_runtime_condition_cannot_hold_a_jinja_tag() {
    for (when, said) in [
        ("\"{{ grains['os'] }} == 'Debian'\"", "holds \"{{\""),
        ("\"true %}{% include 'x'\"", "holds \"{%\""),
        ("\"a\\nb\"", "spans more than one line"),
        ("\"  \"", "is blank"),
    ] {
        let err = refusal(&format!(
            "    - id: job\n      type: exec\n      runtimeWhen: {when}\n      params: {{command: /bin/true}}\n"
        ));
        assert_eq!(err.rule, "invalid-runtime-condition", "{when}");
        assert!(err.msg.contains(said), "{}", err.msg);
    }
}

#[test]
fn values_that_look_like_other_types_stay_strings() {
    let text = sls(
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        mode: '0644'\n        owner: 'yes'\n        content: \"a: b\\n# not a comment\\n\"\n",
    );
    assert!(text.contains("- mode: \"0644\""), "{text}");
    assert!(text.contains("- user: \"yes\""), "{text}");
    assert!(
        text.contains("- contents: |\n        a: b\n        # not a comment\n"),
        "{text}"
    );
}

// What Salt itself would refuse, caught first.

#[test]
fn a_document_whose_name_cannot_name_a_state_file_is_refused() {
    let mut graph = fixture("accounts", "salt");
    graph.name = "web.prod".into();
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "invalid-name");
    graph.name = "top".into();
    assert_eq!(emit(&graph).unwrap_err().rule, "reserved-name");
    graph.name.clear();
    assert_eq!(emit(&graph).unwrap_err().rule, "missing-param");
}

#[test]
fn a_host_that_cannot_be_a_minion_id_is_refused() {
    let mut graph = fixture("accounts", "salt");
    graph.host.clear();
    assert_eq!(emit(&graph).unwrap_err().rule, "missing-host");
    graph.host = "web01,web02".into();
    assert_eq!(emit(&graph).unwrap_err().rule, "invalid-host");
}

#[test]
fn an_id_salt_reads_for_itself_is_refused() {
    let mut graph = fixture("accounts", "salt");
    graph.resources.truncate(1);
    graph.edges.clear();
    graph.resources[0].id = "include".into();
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "reserved-name");
    assert_eq!(err.resource, "include");
}

#[test]
fn a_resource_named_like_a_reaction_is_refused() {
    let resources = format!(
        "{}{SVC}    - id: reload svc\n      type: package\n      params: {{name: x}}\n",
        CONF.replace("ACTION", "reload")
    );
    let err = refusal(&resources);
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(
        err.msg,
        "state ID \"reload svc\" is already used by resource \"reload svc\""
    );
}

#[test]
fn a_tree_whose_order_breaks_a_dependency_is_refused() {
    let mut graph = fixture("restart_chain", "salt");
    graph.resources.reverse();
    let err = emit(&graph).expect_err("the order contradicts the edges");
    assert_eq!(err.rule, "ordering-violated");
}

#[test]
fn a_dependency_cycle_is_refused() {
    let mut graph = without_positions(fixture("accounts", "salt"));
    let (first, last) = (
        graph.resources[0].id.clone(),
        graph.resources.last().unwrap().id.clone(),
    );
    graph.edges.push(Edge {
        from: last,
        to: first,
        kind: EdgeKind::HardOrder,
        action: String::new(),
        pos: Position::default(),
    });
    assert_eq!(emit(&graph).unwrap_err().rule, "dependency-cycle");
}

#[test]
fn an_edge_to_a_resource_that_is_not_there_is_refused() {
    let mut graph = without_positions(fixture("accounts", "salt"));
    let first = graph.resources[0].id.clone();
    graph.edges.push(Edge {
        from: first,
        to: "ghost".into(),
        kind: EdgeKind::Notify,
        action: "restart".into(),
        pos: Position::default(),
    });
    assert_eq!(emit(&graph).unwrap_err().rule, "unknown-notify-target");
}

#[test]
fn a_reference_is_settled_from_the_document_or_refused() {
    let text = sls(
        "    - id: root\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: page\n      type: file\n      params:\n        path: ${root.path}/index.html\n        content: \"costs $${PRICE}\"\n",
    );
    assert!(text.contains("/srv/app/index.html"), "{text}");
    assert!(text.contains("costs ${PRICE}"), "{text}");
    assert!(!text.contains("${root"), "{text}");
    assert!(!text.contains("$${"), "{text}");

    let err = refusal(
        "    - id: page\n      type: file\n      params:\n        path: ${nowhere.path}/index.html\n        content: x\n",
    );
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "page");
    assert!(err.msg.contains("does not declare"), "{}", err.msg);
}

// ---- secrets ------------------------------------------------------------

/// A file and a command, with a secret in the param named for each.
fn with_secrets(resources: &str, places: &[(&str, &str, &str)]) -> ResourceGraph {
    let mut graph = graph_from_text("site.yaml", &document(resources), "salt");
    for (id, param, name) in places {
        secret_in_param(&mut graph, id, param, name);
    }
    graph
}

#[test]
fn a_secret_in_a_param_is_read_from_the_pillar_and_the_state_shows_no_change() {
    let graph = with_secrets(
        "    - id: env_file\n      type: file\n      params: {path: /etc/app/env, content: \"a={x}\\n\", mode: \"0600\"}\n",
        &[("env_file", "content", "db_password")],
    );
    let text = emit(&graph).unwrap().artifact.files[SLS].clone();
    assert!(text.starts_with("#!jinja|yaml\n"), "{text}");
    assert!(
        text.contains(
            "    - contents: {{ pillar['meridian_secret_db_password'] | yaml_encode }}\n"
        ),
        "{text}"
    );
    assert!(text.contains("    - show_changes: false\n"), "{text}");
    assert!(!text.contains("a={x}"), "{text}");
    assert_eq!(
        meridian_target_salt::secret_reference("db_password"),
        "{{ pillar['meridian_secret_db_password'] | yaml_encode }}"
    );
    assert_eq!(
        meridian_target_salt::secret_key("db_password"),
        "meridian_secret_db_password"
    );
    assert!(Salt.capabilities().secrets);
}

#[test]
fn a_command_given_a_secret_is_refused_because_salt_prints_the_command_line() {
    let graph = with_secrets(
        "    - id: login\n      type: exec\n      params: {command: \"db-login --token placeholder\"}\n",
        &[("login", "command", "api_token")],
    );
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-secret-place");
    assert_eq!(err.resource, "login");
    assert!(err.msg.contains("Salt prints the command line"), "{err}");
}

#[test]
fn other_braces_in_a_file_rendered_for_a_secret_stay_literal() {
    let graph = with_secrets(
        "    - id: literal\n      type: file\n      params: {path: /etc/app/tpl, content: \"{{ not jinja }}\\n\"}\n    - id: env\n      type: file\n      params: {path: /etc/app/env, content: placeholder}\n",
        &[("env", "content", "api_token")],
    );
    let text = emit(&graph).unwrap().artifact.files[SLS].clone();
    assert!(text.starts_with("#!jinja|yaml\n"), "{text}");
    assert!(
        text.contains("    - contents: {{ pillar['meridian_secret_api_token'] | yaml_encode }}\n"),
        "{text}"
    );
    // The other file is rendered by Jinja now too, so its braces are
    // escaped: Salt reads the same bytes the document wrote.
    assert!(
        !text.contains("- contents: |\n        {{ not jinja }}"),
        "{text}"
    );
    assert!(text.contains("{{meridian_brace}}{ not jinja }}"), "{text}");
}

#[test]
fn a_secret_the_graph_does_not_name_is_written_as_its_mask() {
    let mut graph = with_secrets(
        "    - id: env_file\n      type: file\n      params: {path: /etc/app/env, content: x}\n",
        &[("env_file", "content", "db_password")],
    );
    graph.secrets.clear();
    let text = emit(&graph).unwrap().artifact.files[SLS].clone();
    assert!(!text.contains("pillar["), "{text}");
}

// ---- the credential a host is reached with -----------------------------

/// A document reaching its host as the credential `entry` (a YAML mapping
/// on one line), with the secrets it may read declared.
fn with_credential(entry: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: salt\n  targetHost:\n    from: inline\n    host: web01.example.com\n    credential: deploy\n  secrets:\n    - {{name: deploy_key, from: env, key: DEPLOY_KEY}}\n    - {{name: deploy_password, from: env, key: DEPLOY_PASSWORD}}\n  credentials:\n    - {entry}\n  resources:\n    - id: nginx_pkg\n      type: package\n      params: {{name: nginx}}\n"
    );
    emit(&graph_from_text("site.yaml", &text, "salt"))
}

#[test]
fn a_credential_salt_ssh_can_take_is_accepted_and_leaves_nothing_in_the_states() {
    for entry in [
        "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\"}",
        "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\", passphrase: \"${{ secret.deploy_password }}\"}",
        "{name: deploy, type: ssh, user: deploy, password: \"${{ secret.deploy_password }}\"}",
        "{name: deploy, type: usernamePassword, user: deploy, password: \"${{ secret.deploy_password }}\"}",
        "{name: deploy, type: ssh, agent: true}",
        "{name: deploy, type: ssh, from: ambient, user: ops}",
    ] {
        let emitted = with_credential(entry).unwrap_or_else(|e| panic!("{entry}: {e}"));
        // The runner writes the credential into salt-ssh's roster; the
        // state files hold nothing of it.
        for (file, text) in &emitted.artifact.files {
            assert!(!text.contains("deploy"), "{file} names the credential:\n{text}");
        }
    }
}

#[test]
fn what_salt_ssh_cannot_take_of_a_credential_is_refused_with_the_reason() {
    for (entry, reason) in [
        (
            "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\", certificate: cert.pub}",
            "salt-ssh takes no certificate for a key from Meridian",
        ),
        (
            "{name: deploy, type: usernamePassword, user: admin, password: \"${{ secret.deploy_password }}\", domain: CORP}",
            "salt-ssh over SSH takes no domain",
        ),
        (
            "{name: deploy, type: ssh, user: \"dep loy\", privateKey: \"${{ secret.deploy_key }}\"}",
            "cannot be given to salt-ssh",
        ),
    ] {
        let err = with_credential(entry).expect_err(entry);
        assert_eq!(err.rule, "unsupported-credential", "{entry}: {err}");
        assert!(err.msg.contains(reason), "{entry}: {err}");
        assert!(err.msg.starts_with("the credential \"deploy\""), "{err}");
    }
}

// ---- Salt's own state functions beside the six ----------------------------

/// The document behind the `native_states` golden files: Salt's own state
/// functions beside the shared ones, with a condition, notified and
/// notifying, and one with no name of its own.
const NATIVE_STATES: &str = "    - id: nginx_repo\n      type: pkgrepo.managed\n      params:\n        name: deb http://nginx.org/packages/debian bookworm nginx\n        file: /etc/apt/sources.list.d/nginx.list\n        key_url: https://nginx.org/keys/nginx_signing.key\n      runtimeWhen: grains['os_family'] == 'Debian'\n    - id: nginx_pkg\n      type: package\n      params:\n        name: nginx\n      dependsOn:\n        - resource: nginx_repo\n    - id: nginx_conf\n      type: file.symlink\n      params:\n        name: /etc/nginx/sites-enabled/web\n        target: /etc/nginx/sites-available/web\n      dependsOn:\n        - resource: nginx_pkg\n      notifies:\n        - resource: nginx_svc\n          action: restart\n        - resource: app\n          action: watch\n    - id: nginx_svc\n      type: service\n      params:\n        name: nginx\n    - id: app\n      type: docker_container.running\n      params:\n        image: app:1.2\n        port_bindings: ['8080:80']\n";

#[test]
fn a_state_function_is_written_with_its_arguments_as_written() {
    let emitted = compile(NATIVE_STATES).unwrap_or_else(|e| panic!("{e}"));
    assert_golden(
        &repo_root().join("plugins/salt/testdata"),
        "native_states",
        &emitted,
    );
    let text = &emitted.artifact.files[SLS];
    // The arguments are the function's own, as written, name first.
    assert!(
        text.contains("nginx_repo:\n  pkgrepo.managed:\n    - name: deb http://nginx.org/packages/debian bookworm nginx\n    - file: /etc/apt/sources.list.d/nginx.list\n    - key_url: https://nginx.org/keys/nginx_signing.key\n"),
        "{text}"
    );
    // A state with no name of its own is named by its ID, as Salt reads it.
    assert!(
        text.contains(
            "app:\n  docker_container.running:\n    - image: app:1.2\n    - port_bindings:\n"
        ),
        "{text}"
    );
    // A notified state watches the notifier.
    assert!(
        text.contains("    - watch:\n        - file: nginx_conf\n"),
        "{text}"
    );
    assert!(
        text.contains("    - require:\n        - pkg: nginx_pkg\n"),
        "{text}"
    );
}

#[test]
fn a_state_function_takes_no_state_and_can_only_watch_when_notified() {
    let err = refusal(
        "    - id: a\n      type: file.symlink\n      state: present\n      params:\n        name: /x\n        target: /y\n",
    );
    assert_eq!(err.rule, "unsupported-state");

    let err = refusal("    - id: a\n      type: file.symlink\n      params:\n        target: ~\n");
    assert_eq!(err.rule, "empty-param");

    let err = refusal(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: a\n          action: restart\n    - id: a\n      type: docker_container.running\n      params: {image: app}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "a");
    assert_eq!(
        err.msg,
        "resource type \"docker_container.running\" is a Salt state, which can only watch \
         the notifier when notified (action \"watch\"), got action \"restart\""
    );
}

#[test]
fn a_watch_from_a_state_applied_later_gets_a_state_after_both() {
    let text = sls(
        "    - id: app\n      type: docker_container.running\n      params: {image: app}\n    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      dependsOn:\n        - resource: app\n      notifies:\n        - resource: app\n          action: watch\n",
    );
    assert!(
        text.ends_with(
            "watch app:\n  docker_container.running:\n    - image: app\n    - require:\n        - docker_container: app\n    - watch:\n        - file: conf\n"
        ),
        "{text}"
    );
}

#[test]
fn a_name_that_is_not_a_state_function_is_refused_with_the_dotted_name_to_write() {
    for r#type in [
        "File.Managed",
        "pkgrepo",
        "a.b.c",
        "mysql::db",
        "native/cron",
    ] {
        let err = refusal(&format!(
            "    - id: a\n      type: '{type}'\n      params:\n        name: /x\n"
        ));
        assert_eq!(err.rule, "unsupported-resource-type", "{type}");
    }
    let err = refusal("    - id: a\n      type: native/file\n      params:\n        name: /x\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(
        err.msg,
        "resource type \"native/file\" is not how a state is reached on this target: a Salt \
         state is written by its dotted name, file.managed or another function of its module"
    );
}

#[test]
fn a_secret_handed_to_a_state_function_is_read_from_the_pillar_and_a_command_is_refused_one() {
    let graph = with_secrets(
        "    - id: key\n      type: file.managed\n      params: {name: /etc/app/key, contents: placeholder, mode: '0600'}\n",
        &[("key", "contents", "api_key")],
    );
    let text = emit(&graph).unwrap().artifact.files[SLS].clone();
    assert!(
        text.contains("    - contents: {{ pillar['meridian_secret_api_key'] | yaml_encode }}\n"),
        "{text}"
    );
    assert!(text.contains("    - show_changes: false\n"), "{text}");
    assert!(!text.contains("placeholder"), "{text}");

    let graph = with_secrets(
        "    - id: login\n      type: cmd.run\n      params: {name: 'docker login -p placeholder'}\n",
        &[("login", "name", "api_key")],
    );
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-secret-place");
}

#[test]
fn the_schemas_offer_any_state_function_as_an_open_pattern() {
    let schemas = Salt.resource_schemas();
    let pattern = schemas
        .iter()
        .find(|s| s.r#type == meridian_target_salt::NATIVE_PATTERN)
        .expect("the function pattern is declared");
    assert!(pattern.open && pattern.is_pattern() && pattern.params.is_empty());
    assert_eq!(pattern.actions, ["watch"]);
}

#[test]
fn a_repository_is_a_pkgrepo_state() {
    let apt = sls("    - id: repo\n      type: packageRepository\n      params:\n        name: nginx\n        format: apt\n        url: https://nginx.org/packages/debian\n        distribution: bookworm\n        components: [nginx]\n        key: https://nginx.org/keys/nginx_signing.key\n");
    assert!(apt.contains("pkgrepo.managed:"), "{apt}");
    for line in [
        "name: deb https://nginx.org/packages/debian bookworm nginx",
        "file: /etc/apt/sources.list.d/nginx.list",
        "key_url: https://nginx.org/keys/nginx_signing.key",
    ] {
        assert!(apt.contains(line), "{line} in {apt}");
    }
    let yum = sls("    - id: repo\n      type: packageRepository\n      state: absent\n      params:\n        name: nginx\n        format: yum\n        url: https://nginx.org/packages/rhel/9/x86_64/\n");
    assert!(yum.contains("pkgrepo.absent:"), "{yum}");
    assert!(!yum.contains("baseurl"), "{yum}");
}
