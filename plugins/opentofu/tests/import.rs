//! Importing an existing OpenTofu configuration, all the way round.

#![allow(clippy::result_large_err)]

use meridian_import::diff::{assert_same, Rules};
use meridian_plugin_testkit::{import_text, json, notes_text, round_trip};
use meridian_sdk::Importer;
use meridian_target_opentofu::{OpenTofu, OpenTofuImporter, CONFIG_PATH};

/// What OpenTofu does not tell apart in its JSON syntax.
const OPENTOFU: Rules = Rules {
    null_is_absent: false,
    empty_is_absent: true,
    one_block_is_a_list: true,
};

const CONFIGURATION: &str = r#"
terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }

  encryption {
    key_provider "pbkdf2" "main" {
      passphrase = var.passphrase
    }
    method "aes_gcm" "main" {
      keys = key_provider.pbkdf2.main
    }
    state {
      method = method.aes_gcm.main
    }
  }
}

provider "aws" {
  region = var.region
}

variable "region" {
  type    = string
  default = "eu-west-1"
}

variable "passphrase" {
  type      = string
  sensitive = true
}

locals {
  name = "web-${var.region}"
}

ephemeral "aws_secretsmanager_secret_version" "db" {
  secret_id = "db-password"
}

resource "aws_instance" "web" {
  ami           = "ami-123"
  instance_type = "t3.micro"
  tags = {
    Name = local.name
  }
}

resource "aws_eip" "ip" {
  instance   = aws_instance.web.id
  depends_on = [aws_instance.web]
}

output "address" {
  value = aws_eip.ip.public_ip
}
"#;

const EXPECTED: &str = r#"
{
  "terraform": {
    "required_providers": {"aws": {"source": "hashicorp/aws", "version": "~> 5.0"}},
    "encryption": {
      "key_provider": {"pbkdf2": {"main": {"passphrase": "${var.passphrase}"}}},
      "method": {"aes_gcm": {"main": {"keys": "${key_provider.pbkdf2.main}"}}},
      "state": {"method": "method.aes_gcm.main"}
    }
  },
  "provider": {"aws": {"region": "${var.region}"}},
  "variable": {
    "region": {"type": "string", "default": "eu-west-1"},
    "passphrase": {"type": "string", "sensitive": true}
  },
  "locals": {"name": "web-${var.region}"},
  "ephemeral": {
    "aws_secretsmanager_secret_version": {"db": {"secret_id": "db-password"}}
  },
  "resource": {
    "aws_instance": {
      "web": {"ami": "ami-123", "instance_type": "t3.micro", "tags": {"Name": "${local.name}"}}
    },
    "aws_eip": {"ip": {"instance": "${aws_instance.web.id}"}}
  },
  "output": {"address": {"value": "${aws_eip.ip.public_ip}"}}
}
"#;

#[test]
fn a_configuration_comes_back_meaning_the_same() {
    // The file has OpenTofu's own extension, and uses what only OpenTofu
    // has: state encryption and an ephemeral resource.
    let source = import_text("web", &[("main.tofu", CONFIGURATION)]);
    let (imported, trips) = round_trip(&OpenTofuImporter, &OpenTofu, &source);
    assert_eq!(imported.needing_attention(), 0, "{}", notes_text(&imported));
    assert_eq!(imported.documents[0].target, "opentofu");
    let compiled = &trips[0].emitted.artifact.files[CONFIG_PATH];
    // The one difference that means nothing: the reference from the address
    // to the machine already orders them, so `depends_on` is not repeated.
    assert_same(&json(EXPECTED), &json(compiled), OPENTOFU);
}

#[test]
fn the_formats_cover_both_extensions_opentofu_reads() {
    let formats = OpenTofuImporter.formats();
    let claims = |name: &str| formats.iter().any(|f| f.matches(name));
    for name in ["main.tofu", "main.tf", "main.tofu.json", "main.tf.json"] {
        assert!(claims(name), "{name}");
    }
    assert!(!claims("values.tfvars"));
}

#[test]
fn a_tofu_file_is_read_instead_of_the_tf_file_beside_it() {
    let source = import_text(
        "x",
        &[
            ("main.tf", "resource \"null_resource\" \"old\" {}\n"),
            ("main.tofu", "resource \"null_resource\" \"new\" {}\n"),
            ("other.tf", "resource \"null_resource\" \"kept\" {}\n"),
        ],
    );
    let imported = OpenTofuImporter.import(&source).unwrap();
    let ids: Vec<&str> = imported.documents[0]
        .resources
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, ["new", "kept"]);
    assert_eq!(imported.needing_attention(), 0);
    assert!(
        imported.notes[0].msg.contains("reads main.tofu instead"),
        "{}",
        imported.notes[0].msg
    );
}
