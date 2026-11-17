//! Importing an existing Terraform configuration, all the way round: the
//! configuration is imported, the document is written and read back, the
//! document is compiled, and what it compiles to is compared, by meaning,
//! with the configuration that was imported.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_import::diff::{assert_same, Rules};
use meridian_plugin_testkit::{
    assert_golden_text, import_source, import_text, json, notes_text, round_trip,
};
use meridian_sdk::{Importer, NoteLevel, Value};
use meridian_target_terraform::{Terraform, TerraformImporter, CONFIG_PATH};

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/import")
}

/// What Terraform does not tell apart in its JSON syntax: a nested block is
/// an object or a list of one, an empty block is no block, and the comment
/// key is not configuration.
const TERRAFORM: Rules = Rules {
    null_is_absent: false,
    empty_is_absent: true,
    one_block_is_a_list: true,
};

fn without_comment(mut value: Value) -> Value {
    if let Value::Map(map) = &mut value {
        map.remove("//");
    }
    value
}

#[test]
fn a_configuration_in_hcl_comes_back_meaning_the_same() {
    let source = import_source(&testdata().join("web-vm"), "web-vm");
    let (imported, trips) = round_trip(&TerraformImporter, &Terraform, &source);
    assert_eq!(trips.len(), 1);

    // The document `meridian import` writes, and what it told the operator.
    assert_golden_text(&testdata(), "web-vm.meridian.yaml", &trips[0].rendered.text);
    assert_golden_text(&testdata(), "web-vm.notes", &notes_text(&imported));

    // The semantic diff. The expected file is the same configuration written
    // by hand in the JSON syntax, with the two renames the notes report.
    let expected = std::fs::read_to_string(testdata().join("web-vm.expected.tf.json")).unwrap();
    let compiled = &trips[0].emitted.artifact.files[CONFIG_PATH];
    assert_same(
        &without_comment(json(&expected)),
        &json(compiled),
        TERRAFORM,
    );
}

#[test]
fn a_configuration_in_the_json_syntax_comes_back_unchanged() {
    // What the Terraform target itself writes for its example is a
    // configuration like any other, so importing it and compiling the result
    // has to give it back.
    let original = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/web-vm.main_tf_json"),
    )
    .unwrap();
    let source = import_text("web-vm", &[("main.tf.json", &original)]);
    let (imported, trips) = round_trip(&TerraformImporter, &Terraform, &source);
    assert_eq!(imported.needing_attention(), 0, "{}", notes_text(&imported));
    let compiled = &trips[0].emitted.artifact.files[CONFIG_PATH];
    assert_same(&json(&original), &json(compiled), TERRAFORM);
}

#[test]
fn what_cannot_be_translated_is_reported_and_marked() {
    let source = import_text(
        "odd",
        &[(
            "main.tf",
            r#"
terraform {
  required_version = "~> 1.9"
  cloud {
    organization = "example"
  }
}

import {
  to = terraform_data.a
  id = "a"
}

check "health" {
  assert {
    condition     = terraform_data.a.output == "x"
    error_message = "unhealthy"
  }
}

resource "terraform_data" "a" {
  input = "%{ if var.loud }LOUD%{ else }quiet%{ endif }"
}

variable "loud" {
  default = false
}

output "a" {
  value       = terraform_data.a.output
  sensitive   = true
  description = "The value."
}
"#,
        )],
    );
    let (imported, trips) = round_trip(&TerraformImporter, &Terraform, &source);
    let lost: Vec<&str> = imported
        .notes
        .iter()
        .filter(|n| n.level == NoteLevel::Untranslated)
        .map(|n| n.subject.as_str())
        .collect();
    assert_eq!(
        lost,
        [
            "terraform.cloud",
            "terraform.required_version",
            "import block",
            "check \"health\"",
            "resource \"terraform_data\" \"a\"",
            "output \"a\"",
            "output \"a\"",
        ]
    );
    // Every one of them is a marker in the file, so none can be missed.
    let text = &trips[0].rendered.text;
    assert_eq!(trips[0].rendered.todos, 7);
    assert_eq!(
        text.matches("TODO(meridian-import): not translated")
            .count(),
        7
    );
    // The one that is about a resource sits beside it.
    let marker = text.find("uses a %{ } template directive").unwrap();
    let resource = text.find("- id: a\n").unwrap();
    assert!(marker < resource && resource - marker < 400, "{text}");
}

#[test]
fn a_file_that_is_not_a_configuration_is_refused_with_its_line() {
    let broken = import_text("x", &[("main.tf", "resource \"a_b\" \"c\" {\n  d = [1,\n")]);
    let err = TerraformImporter.import(&broken).unwrap_err();
    assert_eq!((err.file.as_str(), err.line), ("main.tf", 2), "{err}");

    let values = import_text("x", &[("main.tf", "region = \"us-east-1\"\n")]);
    let err = TerraformImporter.import(&values).unwrap_err();
    assert!(err.msg.contains("outside any block"), "{err}");

    let nothing = import_text("x", &[("prod.tfvars", "region = \"us-east-1\"\n")]);
    let err = TerraformImporter.import(&nothing).unwrap_err();
    assert!(err.msg.contains("no file ends in .tf"), "{err}");

    let other = import_text("x", &[("playbook.yml", "- hosts: all\n")]);
    let err = TerraformImporter.import(&other).unwrap_err();
    assert!(
        err.msg.contains("not a Terraform configuration file"),
        "{err}"
    );
}

#[test]
fn the_formats_say_which_files_are_terraforms() {
    let formats = TerraformImporter.formats();
    let claims = |name: &str| formats.iter().any(|f| f.matches(name));
    assert!(claims("main.tf"));
    assert!(claims("main.tf.json"));
    assert!(!claims("main.tofu"));
    assert!(!claims("terraform.tfvars"));
    assert!(!claims("compose.yaml"));
}
