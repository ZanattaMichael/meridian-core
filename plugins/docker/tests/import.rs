//! Importing an existing Compose file, all the way round: the file is
//! imported, the document is written and read back, the document is
//! compiled, and the Compose file it compiles to is compared, by meaning,
//! with the one that was imported.

#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use meridian_import::diff::{assert_same, Rules};
use meridian_import::yaml;
use meridian_plugin_testkit::{
    assert_golden_text, import_source, import_text, notes_text, round_trip,
};
use meridian_sdk::{Importer, NoteLevel, Value};
use meridian_target_docker::{Docker, DockerImporter, COMPOSE_PATH};

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/import")
}

/// What Compose does not tell apart: an element with no fields is written
/// with nothing after its name or with an empty mapping.
const COMPOSE: Rules = Rules {
    null_is_absent: true,
    empty_is_absent: true,
    one_block_is_a_list: false,
};

/// A Compose file as Compose reads it, with the two spellings of
/// `depends_on` made one: the short form, a list of names, means each has to
/// have started.
fn compose(text: &str) -> Value {
    let documents = yaml::read("compose.yaml", text, yaml::Rules::Yaml12).unwrap();
    let Value::Map(mut top) = documents[0].value() else {
        panic!("a Compose file is a mapping");
    };
    // Not part of the project: extension fields, and the obsolete version.
    top.retain(|key, _| !key.starts_with("x-") && key != "version");
    if let Some(Value::Map(services)) = top.get_mut("services") {
        for service in services.values_mut() {
            let Value::Map(service) = service else {
                continue;
            };
            if let Some(Value::List(names)) = service.get("depends_on").cloned() {
                let long = names
                    .into_iter()
                    .filter_map(|name| {
                        Some((
                            name.as_str()?.to_owned(),
                            Value::Map(BTreeMap::from([(
                                "condition".to_owned(),
                                Value::String("service_started".to_owned()),
                            )])),
                        ))
                    })
                    .collect();
                service.insert("depends_on".to_owned(), Value::Map(long));
            }
        }
    }
    Value::Map(top)
}

#[test]
fn a_compose_file_comes_back_meaning_the_same() {
    let source = import_source(&testdata().join("shop"), "ignored");
    let (imported, trips) = round_trip(&DockerImporter, &Docker, &source);
    assert_eq!(trips.len(), 1);
    assert_eq!(imported.needing_attention(), 0, "{}", notes_text(&imported));
    assert_golden_text(&testdata(), "shop.meridian.yaml", &trips[0].rendered.text);
    assert_golden_text(&testdata(), "shop.notes", &notes_text(&imported));

    // The semantic diff.
    let original = &source.files["compose.yaml"];
    let compiled = &trips[0].emitted.artifact.files[COMPOSE_PATH];
    assert_same(&compose(original), &compose(compiled), COMPOSE);

    // The project's name is the document's name.
    assert_eq!(imported.documents[0].name, "shop");
    // What a document says its own way was translated, and only that.
    let resource = |id: &str| {
        imported.documents[0]
            .resources
            .iter()
            .find(|r| r.id == id)
            .unwrap()
    };
    assert_eq!(resource("migrate").depends_on, ["db"]);
    assert_eq!(resource("proxy").depends_on, ["app"]);
    assert_eq!(resource("app").depends_on, ["cache"]);
    assert_eq!(resource("db").notifies[0].resource, "app");
    assert_eq!(resource("db").notifies[0].action, "restart");
    assert_eq!(
        resource("app").params["depends_on"].canonical(),
        r#"{"migrate":{"condition":"service_completed_successfully"}}"#
    );
}

#[test]
fn the_directory_the_file_was_in_becomes_the_projects() {
    let mut source = import_text(
        "blog",
        &[(
            "compose.yaml",
            "services:\n  web:\n    build: ./web\n    env_file: .env\n    volumes:\n      - ./data:/data\n",
        )],
    );
    source.root = "/srv/blog".to_owned();
    let (imported, trips) = round_trip(&DockerImporter, &Docker, &source);
    let project = &imported.documents[0].resources[0];
    assert_eq!(project.r#type, "project");
    assert_eq!(
        project.params["directory"],
        Value::String("/srv/blog".to_owned())
    );
    // With the directory known the target has nothing to warn about.
    assert!(
        trips[0].emitted.warnings.is_empty(),
        "{:?}",
        trips[0].emitted.warnings
    );
    assert_eq!(imported.documents[0].name, "blog");
}

#[test]
fn what_a_document_cannot_say_is_reported_and_marked() {
    let source = import_text(
        "odd",
        &[(
            "compose.yaml",
            r#"
version: "3.8"
include:
  - other.yaml
services:
  web:
    image: "example/web:${TAG:-latest}"
    environment:
      HOME_DIR: $HOME
    x-team: payments
  db:
    image: postgres:16
  my.service:
    image: busybox
volumes:
  db:
"#,
        )],
    );
    let (imported, trips) = round_trip(&DockerImporter, &Docker, &source);
    let lost: Vec<&str> = imported
        .notes
        .iter()
        .filter(|n| n.level == NoteLevel::Untranslated)
        .map(|n| n.subject.as_str())
        .collect();
    assert_eq!(
        lost,
        [
            "service \"my.service\"",
            "volume \"db\"",
            "include",
            "service \"web\""
        ]
    );
    assert_eq!(trips[0].rendered.todos, 4);
    let variables = imported
        .notes
        .iter()
        .find(|n| n.resource == "web" && n.level == NoteLevel::Untranslated)
        .unwrap();
    assert!(variables.msg.contains("$HOME, ${TAG}"), "{}", variables.msg);
    // The text is carried literally, which is what the marker says.
    let compiled = &trips[0].emitted.artifact.files[COMPOSE_PATH];
    assert!(
        compiled.contains("example/web:$${TAG:-latest}"),
        "{compiled}"
    );
}

#[test]
fn a_file_that_is_not_one_compose_file_is_refused() {
    let two = import_text(
        "x",
        &[
            ("compose.yaml", "services: {}\n"),
            ("compose.override.yaml", "services: {}\n"),
        ],
    );
    let err = DockerImporter.import(&two).unwrap_err();
    assert!(err.msg.contains("docker compose -f"), "{err}");

    let manifest = import_text("x", &[("compose.yaml", "apiVersion: v1\nkind: Pod\n")]);
    let err = DockerImporter.import(&manifest).unwrap_err();
    assert!(err.msg.contains("no services"), "{err}");

    let broken = import_text("x", &[("compose.yaml", "services:\n  web: [1,\n")]);
    let err = DockerImporter.import(&broken).unwrap_err();
    assert_eq!(err.file, "compose.yaml");
}

#[test]
fn the_formats_claim_compose_files_by_name() {
    let formats = DockerImporter.formats();
    let claims = |name: &str| formats.iter().any(|f| f.matches(name));
    for name in [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yml",
        "docker-compose.prod.yaml",
        "compose.override.yaml",
    ] {
        assert!(claims(name), "{name}");
    }
    assert!(!claims("deployment.yaml"));
    assert!(!claims("playbook.yml"));
}
