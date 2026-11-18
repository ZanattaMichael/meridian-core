//! The plugin's real binary, driven by the host across the process boundary.
//!
//! The compile tests call the emitter in process. These prove the same
//! answers arrive when the host starts the shipped binary and asks it over
//! the wire: what it is, what it compiles, and what it refuses.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_host::{Client, EmitError, Registry, StartOptions};
use meridian_plugin_testkit::{
    assert_golden, assert_golden_refusal, graph_from_file, plugin_binary,
};
use meridian_sdk::{Capabilities, Category, Data, Operations, PROTOCOL_VERSION, VERSION};

const PACKAGE: &str = "meridian-target-opentofu";

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn start() -> Client {
    let binary = plugin_binary(PACKAGE);
    Client::start(&binary.to_string_lossy(), &StartOptions::default())
        .expect("the OpenTofu plugin starts")
}

#[test]
fn the_binary_declares_its_category_and_everything_it_can_do() {
    let client = start();
    let info = client.info();
    assert_eq!(info.name, "opentofu");
    assert_eq!(info.protocol_version, PROTOCOL_VERSION);
    assert_eq!(info.sdk_version, VERSION);
    assert_eq!(info.category, Category::InfrastructureAsCode);
    assert_eq!(info.operations, Operations::ALL);
    assert_eq!(
        info.capabilities,
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
            secrets: true,
            credentials: false,
            cloud_credentials: true,
            adopt: meridian_sdk::Adoption::Import,
        }
    );
    assert_eq!(
        info.resource_types,
        [
            "<provider>_<type>",
            "address",
            "backend",
            "data.<type>",
            "encryption",
            "ephemeral.<type>",
            "firewall",
            "local",
            "module",
            "network",
            "provider",
            "subnet",
            "variable",
            "vm",
        ]
    );
    client.close().expect("the plugin exits when it is closed");
}

#[test]
fn what_crosses_the_boundary_is_byte_for_byte_the_golden_configuration() {
    let client = start();
    for (case, path) in [
        ("web-vm", "examples/opentofu/web-vm.yaml"),
        ("encrypted-state", "examples/opentofu/encrypted-state.yaml"),
        (
            "every-block",
            "plugins/opentofu/tests/documents/every-block.yaml",
        ),
        ("notify", "plugins/opentofu/tests/documents/notify.yaml"),
    ] {
        let graph = graph_from_file(path, "opentofu");
        let emitted = client
            .emit(&graph, &Data::new())
            .unwrap_or_else(|e| panic!("{case}: {e}"));
        assert_golden(&golden(), case, &emitted);
    }
}

#[test]
fn a_refusal_crosses_the_boundary_with_its_rule_and_its_position() {
    let client = start();
    for case in ["refused-encryption", "refused-reference"] {
        let graph = graph_from_file(
            &format!("plugins/opentofu/tests/documents/{case}.yaml"),
            "opentofu",
        );
        match client.emit(&graph, &Data::new()) {
            Err(EmitError::Refused(refusal)) => {
                assert_eq!(refusal.target, "opentofu");
                assert!(refusal.pos.line > 0, "{case}: the position survived");
                assert_golden_refusal(&golden(), case, &refusal);
            }
            other => panic!("{case}: expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn discovery_finds_the_plugin_by_its_name() {
    let binary = plugin_binary(PACKAGE);
    let dir = binary.parent().expect("the binary is in a directory");
    let mut registry = Registry::new();
    let found = registry
        .discover(&dir.to_string_lossy(), &StartOptions::default())
        .expect("discovery succeeds");
    assert_eq!(
        found.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        ["opentofu"]
    );
    let target = registry.target("opentofu").expect("it is registered");
    assert_eq!(target.info().category, Category::InfrastructureAsCode);
    registry.close().expect("the plugin exits");
}
