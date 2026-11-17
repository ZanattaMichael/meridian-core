//! Importing an existing DSC configuration, all the way round: the
//! configuration is imported, the document is written and read back, the
//! document is compiled, and the runner project it compiles to is compared,
//! by meaning, with what the original declares.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_import::diff::{assert_same, Rules};
use meridian_import::yaml;
use meridian_plugin_testkit::{
    assert_golden_text, import_source, import_text, notes_text, round_trip,
};
use meridian_sdk::{Importer, NoteLevel, Value};
use meridian_target_dsc::{node_path, Dsc, DscImporter, DATUM_PATH};

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(testdata().join(name)).unwrap()
}

/// A file of a runner project as the runner reads it.
fn project(text: &str) -> Value {
    yaml::read("node.yml", text, yaml::Rules::Yaml12).unwrap()[0].value()
}

#[test]
fn a_powershell_configuration_comes_back_meaning_the_same() {
    let source = import_source(&testdata().join("import/iis"), "");
    let (imported, trips) = round_trip(&DscImporter, &Dsc, &source);
    assert_eq!(trips.len(), 1);
    assert_golden_text(
        &testdata().join("import"),
        "iis.meridian.yaml",
        &trips[0].rendered.text,
    );
    assert_golden_text(
        &testdata().join("import"),
        "iis.notes",
        &notes_text(&imported),
    );

    // The document is named after the configuration, and it is DSC v2's.
    let document = &imported.documents[0];
    assert_eq!(document.name, "webserver");
    assert_eq!(document.target_version, "2");
    assert_eq!(document.host, "web01.example.com");
    // The one thing that had to be assumed is how the machine is reached.
    let attention: Vec<&str> = imported
        .notes
        .iter()
        .filter(|n| n.level.needs_attention())
        .map(|n| n.resource.as_str())
        .collect();
    assert_eq!(attention, ["runner"]);

    // The semantic diff, against the configuration written out by hand in
    // the runner's format.
    let files = &trips[0].emitted.artifact.files;
    assert_same(
        &project(&read("import/iis.expected.node.yml")),
        &project(&files[&node_path("webserver")]),
        Rules::default(),
    );
    let datum = project(&files[DATUM_PATH]);
    let settings = datum.as_map().unwrap()["PipelineRunnerSettings"]
        .as_map()
        .unwrap();
    assert_eq!(settings["Engine"], Value::String("DscV2".to_owned()));
    assert_eq!(settings["Target"], Value::String("WinRM".to_owned()));
}

#[test]
fn a_runner_project_comes_back_as_it_was() {
    // What the target compiled from its own example, imported again.
    let datum = read("web-server.Datum_yml");
    let node = read("web-server.Projects_Present_web_yml");
    let source = import_text(
        "",
        &[("Datum.yml", &datum), ("Projects/Present/web.yml", &node)],
    );
    let (imported, trips) = round_trip(&DscImporter, &Dsc, &source);
    assert_eq!(imported.needing_attention(), 0, "{}", notes_text(&imported));
    assert_eq!(imported.documents[0].name, "web");
    let files = &trips[0].emitted.artifact.files;
    assert_same(
        &project(&node),
        &project(&files[&node_path("web")]),
        Rules::default(),
    );
    assert_same(
        &project(&datum),
        &project(&files[DATUM_PATH]),
        Rules::default(),
    );
    // Not only the same meaning: the same bytes.
    assert_eq!(files[&node_path("web")], node);
    assert_eq!(files[DATUM_PATH], datum);
}

#[test]
fn a_dsc_v3_document_comes_back_meaning_the_same() {
    let text = read("import/machine.dsc.yaml");
    let source = import_text("machine", &[("machine.dsc.yaml", &text)]);
    let (imported, trips) = round_trip(&DscImporter, &Dsc, &source);
    assert_eq!(imported.needing_attention(), 0, "{}", notes_text(&imported));
    assert_golden_text(
        &testdata().join("import"),
        "machine.meridian.yaml",
        &trips[0].rendered.text,
    );
    let document = &imported.documents[0];
    assert_eq!(document.target_version, "3");
    assert_eq!(document.host, "localhost");
    assert_eq!(document.resources[1].depends_on, ["launch_key"]);

    let compiled = project(&trips[0].emitted.artifact.files[&node_path("machine")]);
    let expected = project(
        r#"
resources:
  - name: launch_key
    type: Microsoft.Windows/Registry
    properties:
      keyPath: HKLM\Software\Example
      valueName: Launch
      valueData:
        String: "[not an expression]"
  - name: Tooling
    type: Microsoft.WinGet/Package
    dependsOn:
      - Microsoft.Windows/Registry/launch_key
    properties:
      id: Git.Git
      source: winget
"#,
    );
    assert_same(
        &expected,
        &compiled,
        Rules {
            empty_is_absent: true,
            ..Rules::default()
        },
    );
}

#[test]
fn what_a_document_cannot_say_is_reported_and_marked() {
    let source = import_text(
        "",
        &[(
            "Fleet.ps1",
            r#"
Configuration Fleet {
    param([string] $Role, [pscredential] $Credential)
    Import-DscResource -ModuleName ModuleA, ModuleB

    Node $AllNodes.NodeName {
        Thing One {
            Path                 = "$env:ProgramData\app"
            Role                 = $Role
            PsDscRunAsCredential = $Credential
            DependsOn            = '[Thing]Missing'
        }
        if ($Role -eq 'web') {
            WindowsFeature IIS { Name = 'Web-Server' }
        }
    }
}
"#,
        )],
    );
    let (imported, trips) = round_trip(&DscImporter, &Dsc, &source);
    assert_golden_text(
        &testdata().join("import"),
        "fleet.meridian.yaml",
        &trips[0].rendered.text,
    );
    assert_golden_text(
        &testdata().join("import"),
        "fleet.notes",
        &notes_text(&imported),
    );
    // One more marker than the importer's notes: the host nobody named.
    assert_eq!(trips[0].rendered.todos, imported.needing_attention() + 1);
    assert!(trips[0].rendered.text.contains("CHANGE-ME.invalid"));
    let lost = imported
        .notes
        .iter()
        .filter(|n| n.level == NoteLevel::Untranslated)
        .count();
    assert_eq!(lost, 5, "{}", notes_text(&imported));
}

#[test]
fn a_source_with_no_configuration_is_refused() {
    let none = import_text("x", &[("values.yaml", "a: 1\n")]);
    let err = DscImporter.import(&none).unwrap_err();
    assert!(err.msg.contains("no DSC configuration"), "{err}");

    let open = import_text("x", &[("c.ps1", "Configuration C {\n  File F {\n")]);
    let err = DscImporter.import(&open).unwrap_err();
    assert_eq!((err.file.as_str(), err.line), ("c.ps1", 3));
}

#[test]
fn the_formats_claim_configurations_by_name() {
    let formats = DscImporter.formats();
    let claims = |name: &str| formats.iter().any(|f| f.matches(name));
    for name in ["WebServer.ps1", "machine.dsc.yaml", "Datum.yml", "web.yml"] {
        assert!(claims(name), "{name}");
    }
    assert!(!claims("main.tf"));
}
