//! Importing an existing playbook, all the way round: the playbook is
//! imported, the document is written and read back, the document is compiled,
//! and the playbook it compiles to is compared, by meaning, with what the
//! original says.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_import::diff::{assert_same, Rules};
use meridian_import::yaml;
use meridian_plugin_testkit::{
    assert_golden_text, import_source, import_text, notes_text, round_trip,
};
use meridian_sdk::{Importer, NoteLevel, Value};
use meridian_target_ansible::{Ansible, AnsibleImporter, INVENTORY_PATH, PLAYBOOK_PATH};

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/import")
}

/// A playbook as Ansible reads it.
fn playbook(text: &str) -> Value {
    yaml::read("playbook.yml", text, yaml::Rules::Yaml11).unwrap()[0].value()
}

#[test]
fn a_playbook_comes_back_meaning_the_same() {
    let source = import_source(&testdata().join("web"), "web");
    let (imported, trips) = round_trip(&AnsibleImporter, &Ansible, &source);
    assert_eq!(trips.len(), 1);
    assert_eq!(imported.needing_attention(), 0, "{}", notes_text(&imported));
    assert_golden_text(&testdata(), "web.meridian.yaml", &trips[0].rendered.text);
    assert_golden_text(&testdata(), "web.notes", &notes_text(&imported));

    // The semantic diff. A compiled playbook names each task after its
    // resource and writes every module in full, so the original is compared
    // in that spelling: the same play, written the way the target writes it.
    let compiled = &trips[0].emitted.artifact.files[PLAYBOOK_PATH];
    let expected = std::fs::read_to_string(testdata().join("web.expected.playbook.yml")).unwrap();
    assert_same(&playbook(&expected), &playbook(compiled), Rules::default());
    assert_eq!(
        trips[0].emitted.artifact.files[INVENTORY_PATH],
        "[meridian]\nweb01.example.com\n"
    );
}

#[test]
fn what_a_document_cannot_say_is_reported_and_marked() {
    let source = import_text(
        "odd",
        &[(
            "site.yml",
            r#"
- name: Odd
  hosts: webservers
  become: true
  vars:
    packages: [git, curl]
    port: 8080
  roles:
    - common
  tasks:
    - name: Packages
      apt:
        name: "{{ packages }}"
        update_cache: true
      become: true
      tags: [setup]
    - name: Template
      template:
        src: app.conf.j2
        dest: /etc/app.conf
      notify: Restart app
    - name: Enable app
      systemd:
        name: app
        enabled: true
    - name: Open the port
      command: "open-port {{ port | int + 1 }}"
      notify:
        - Restart app
        - Flush
        - Missing
    - name: Each
      debug:
        msg: "{{ item }}"
      loop: "{{ packages }}"
    - include_tasks: more.yml
  handlers:
    - name: Restart app
      service:
        name: app
        state: restarted
    - name: Flush
      command: flush-cache
"#,
        )],
    );
    let (imported, trips) = round_trip(&AnsibleImporter, &Ansible, &source);
    assert_golden_text(&testdata(), "odd.meridian.yaml", &trips[0].rendered.text);
    assert_golden_text(&testdata(), "odd.notes", &notes_text(&imported));

    let document = &imported.documents[0];
    let ids: Vec<&str> = document.resources.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["packages", "template", "enable_app", "open_the_port"]);
    // A task a shared type cannot carry whole is the module itself, with
    // every argument, and the document says it is now written for Ansible.
    let packages = &document.resources[0];
    assert_eq!(packages.r#type, "ansible.builtin.apt");
    assert_eq!(packages.state, "");
    assert!(packages.params.contains_key("update_cache"));
    let template = &document.resources[1];
    assert_eq!(template.r#type, "ansible.builtin.template");
    assert_eq!(template.params.keys().collect::<Vec<_>>(), ["dest", "src"]);
    assert!(imported.notes.iter().any(|n| n.level == NoteLevel::Changed
        && n.resource == "template"
        && n.msg.contains("compiles for Ansible only")));
    // The handler that could be translated was, onto the service it acts on.
    let port = &document.resources[3];
    assert_eq!(port.notifies.len(), 1);
    assert_eq!(port.notifies[0].resource, "enable_app");
    assert_eq!(port.notifies[0].action, "restart");
    // Every marker in the document is a note that needs attention.
    assert_eq!(trips[0].rendered.todos, imported.needing_attention());
    assert!(imported
        .notes
        .iter()
        .any(|n| n.level == NoteLevel::Changed && n.resource == "enable_app"));
}

#[test]
fn each_play_is_a_document_and_a_task_file_has_no_host() {
    let source = import_text(
        "site",
        &[
            (
                "site.yml",
                "- name: Web\n  hosts: web01.example.com\n  tasks:\n    - package: {name: nginx}\n\
                 - name: Database\n  hosts: db01.example.com\n  tasks:\n    - package: {name: postgresql}\n",
            ),
            (
                "tasks.yml",
                "- name: Install git\n  package:\n    name: git\n",
            ),
        ],
    );
    let (imported, trips) = round_trip(&AnsibleImporter, &Ansible, &source);
    let names: Vec<(&str, &str)> = imported
        .documents
        .iter()
        .map(|d| (d.name.as_str(), d.host.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("site-web", "web01.example.com"),
            ("site-database", "db01.example.com"),
            ("site-play", ""),
        ]
    );
    // A task with no name is named after what it does.
    assert_eq!(imported.documents[0].resources[0].id, "package_nginx");
    // The document with no host says so, and still compiles.
    assert_eq!(trips[2].rendered.todos, 1);
    assert!(trips[2].rendered.text.contains("CHANGE-ME.invalid"));
}

#[test]
fn a_module_without_a_known_full_name_is_left_out_and_a_full_name_is_kept() {
    let source = import_text(
        "names",
        &[(
            "site.yml",
            "- name: Names\n  hosts: web01\n  tasks:\n    - name: Firewall\n      community.general.ufw:\n        rule: allow\n        port: '80'\n    - name: Legacy copy\n      ansible.legacy.lineinfile:\n        path: /etc/motd\n        line: hi\n    - name: From a role\n      my_custom_module:\n        thing: 1\n    - name: Raw\n      raw: uptime\n",
        )],
    );
    let (imported, _) = round_trip(&AnsibleImporter, &Ansible, &source);
    let document = &imported.documents[0];
    let types: Vec<&str> = document
        .resources
        .iter()
        .map(|r| r.r#type.as_str())
        .collect();
    assert_eq!(
        types,
        ["community.general.ufw", "ansible.builtin.lineinfile"]
    );
    let left_out: Vec<&str> = imported
        .notes
        .iter()
        .filter(|n| n.level == NoteLevel::Untranslated)
        .map(|n| n.msg.as_str())
        .collect();
    assert_eq!(left_out.len(), 2, "{left_out:?}");
    assert!(
        left_out[0].contains("namespace.collection.my_custom_module"),
        "{}",
        left_out[0]
    );
    assert!(left_out[1].contains("free-form"), "{}", left_out[1]);
}

#[test]
fn a_file_that_is_not_a_playbook_is_refused() {
    let vars = import_text("x", &[("vars.yml", "port: 8080\n")]);
    let err = AnsibleImporter.import(&vars).unwrap_err();
    assert!(err.msg.contains("list of plays"), "{err}");

    let broken = import_text("x", &[("site.yml", "- hosts: [a,\n")]);
    let err = AnsibleImporter.import(&broken).unwrap_err();
    assert_eq!(err.file, "site.yml");
}

#[test]
fn the_formats_claim_playbooks_by_name() {
    let formats = AnsibleImporter.formats();
    let claims = |name: &str| formats.iter().any(|f| f.matches(name));
    for name in [
        "site.yml",
        "playbook.yaml",
        "deploy-playbook.yml",
        "web.yml",
    ] {
        assert!(claims(name), "{name}");
    }
    assert!(!claims("main.tf"));
}
