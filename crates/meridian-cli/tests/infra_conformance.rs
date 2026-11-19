//! Conformance of the common infrastructure types: the same document through
//! every target and cloud that maps them.
//!
//! The common types (`network`, `subnet`, `firewall`, `address`, `vm`, `disk`) promise that a
//! resource written once compiles for each infrastructure as code target
//! that takes them, on each cloud it maps them to, "within what each cloud
//! can do". This test writes that promise down cell by cell, the way
//! `conformance.rs` does for the configuration management types: each case
//! is one small document, compiled for every column, and the outcome (`ok`,
//! `ok, warns`, or the rule that refused it) is recorded in
//! `docs/reference/infra-conformance-matrix.md`.
//!
//! A case marked [`Same`] must come out the same in every column. Record the
//! page again with `UPDATE_GOLDEN=1` after a deliberate change.

#![allow(clippy::result_large_err)]

mod support;

use std::fmt::Write as _;
use std::fs;

use meridian_plugin_testkit::{graph_from_text, repo_root, secret_in_param};
use meridian_sdk::{check_settings, CompileError, Emitted};

use support::{emitter, emitters, no_data, outcome};

const PATH: &str = "docs/reference/infra-conformance-matrix.md";

/// One column: a target, and how a document is aimed at a cloud on it.
struct Column {
    title: &'static str,
    target: &'static str,
    /// What goes under `spec:` before `resources:`.
    settings: &'static str,
    /// Resources every document of the column starts with.
    preamble: &'static str,
}

const COLUMNS: &[Column] = &[
    Column {
        title: "terraform (aws)",
        target: "terraform",
        settings: "  settings:\n    cloud: aws\n",
        preamble: "",
    },
    Column {
        title: "terraform (azure)",
        target: "terraform",
        settings:
            "  settings:\n    cloud: azure\n    location: westeurope\n    resourceGroup: rg\n",
        preamble: "",
    },
    Column {
        title: "terraform (gcp)",
        target: "terraform",
        settings: "  settings:\n    cloud: gcp\n    location: europe-west1\n",
        preamble: "",
    },
    Column {
        title: "opentofu (aws)",
        target: "opentofu",
        settings: "  settings:\n    cloud: aws\n",
        preamble: "",
    },
    Column {
        title: "opentofu (azure)",
        target: "opentofu",
        settings:
            "  settings:\n    cloud: azure\n    location: westeurope\n    resourceGroup: rg\n",
        preamble: "",
    },
    Column {
        title: "opentofu (gcp)",
        target: "opentofu",
        settings: "  settings:\n    cloud: gcp\n    location: europe-west1\n",
        preamble: "",
    },
    Column {
        title: "pulumi (aws)",
        target: "pulumi",
        settings: "  settings:\n    cloud: aws\n",
        preamble: "",
    },
    Column {
        title: "pulumi (azure)",
        target: "pulumi",
        settings:
            "  settings:\n    cloud: azure\n    location: westeurope\n    resourceGroup: rg\n",
        preamble: "",
    },
    Column {
        title: "cloudformation",
        target: "cloudformation",
        settings: "",
        preamble: "",
    },
    Column {
        title: "bicep",
        target: "bicep",
        settings: "",
        preamble: "    - id: target\n      type: deployment\n      params: {resourceGroup: rg}\n",
    },
    Column {
        title: "arm",
        target: "arm",
        settings: "",
        preamble: "    - id: target\n      type: deployment\n      params: {resourceGroup: rg}\n",
    },
];

/// The targets that take the common types. A target added here must be
/// given a column.
const COMMON_TARGETS: &[&str] = &[
    "arm",
    "bicep",
    "cloudformation",
    "opentofu",
    "pulumi",
    "terraform",
];

#[derive(Clone, Copy, PartialEq)]
enum Agreement {
    Same,
    Varies,
}
use Agreement::{Same, Varies};

struct Case {
    group: &'static str,
    name: &'static str,
    agreement: Agreement,
    resources: &'static str,
}

const fn case(
    group: &'static str,
    name: &'static str,
    agreement: Agreement,
    resources: &'static str,
) -> Case {
    Case {
        group,
        name,
        agreement,
        resources,
    }
}

const NET: &str = "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16}\n";
const SUBNET: &str =
    "    - id: sub\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24}\n";
const KEY: &str = "ssh-ed25519 AAAAexample";

macro_rules! with_net {
    ($rest:literal) => {
        concat!(
            "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16}\n",
            $rest
        )
    };
}

macro_rules! with_subnet {
    ($rest:literal) => {
        concat!(
            "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16}\n",
            "    - id: sub\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24}\n",
            $rest
        )
    };
}

const CASES: &[Case] = &[
    // ---- network -------------------------------------------------------
    case("network", "a cidr", Same, NET),
    case(
        "network",
        "name and tags",
        Varies,
        "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16, name: prod, tags: {team: web}}\n",
    ),
    case(
        "network",
        "a location",
        Varies,
        "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16, location: westeurope}\n",
    ),
    case(
        "network",
        "no cidr",
        Same,
        "    - id: net\n      type: network\n      params: {name: prod}\n",
    ),
    case(
        "network",
        "a param outside the vocabulary (`dns`)",
        Same,
        "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16, dns: true}\n",
    ),
    case(
        "network",
        "`cidr` is a list",
        Same,
        "    - id: net\n      type: network\n      params: {cidr: [10.0.0.0/16]}\n",
    ),
    case(
        "network",
        "a tag that is not text",
        Same,
        "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16, tags: {count: 3}}\n",
    ),
    case(
        "network",
        "state `absent`",
        Same,
        "    - id: net\n      type: network\n      state: absent\n      params: {cidr: 10.0.0.0/16}\n",
    ),
    // ---- subnet --------------------------------------------------------
    case("subnet", "in a network", Same, with_net!("    - id: sub\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24}\n")),
    case(
        "subnet",
        "`public: true`",
        Varies,
        with_net!("    - id: sub\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24, public: true}\n"),
    ),
    case(
        "subnet",
        "two public subnets in one network",
        Varies,
        with_net!("    - id: a\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24, public: true}\n    - id: b\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.2.0/24, public: true}\n"),
    ),
    case(
        "subnet",
        "a location (availability zone)",
        Varies,
        with_net!("    - id: sub\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24, location: eu-west-1a}\n"),
    ),
    case(
        "subnet",
        "tags",
        Varies,
        with_net!("    - id: sub\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24, tags: {tier: web}}\n"),
    ),
    case(
        "subnet",
        "`network` is a name, not a reference",
        Same,
        with_net!("    - id: sub\n      type: subnet\n      params: {network: net, cidr: 10.0.1.0/24}\n"),
    ),
    case(
        "subnet",
        "`network` refers to nothing",
        Same,
        with_net!("    - id: sub\n      type: subnet\n      params: {network: \"${nowhere}\", cidr: 10.0.1.0/24}\n"),
    ),
    case(
        "subnet",
        "no network",
        Same,
        "    - id: sub\n      type: subnet\n      params: {cidr: 10.0.1.0/24}\n",
    ),
    case(
        "subnet",
        "`public` is text",
        Same,
        with_net!("    - id: sub\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24, public: \"yes\"}\n"),
    ),
    // ---- firewall ------------------------------------------------------
    case(
        "firewall",
        "inbound rules",
        Same,
        with_net!("    - id: fw\n      type: firewall\n      params:\n        network: \"${net}\"\n        rules:\n          - {port: \"80\", cidr: 0.0.0.0/0}\n          - {port: 8000-9000, protocol: udp, cidr: 10.0.0.0/8}\n"),
    ),
    case(
        "firewall",
        "an outbound rule",
        Same,
        with_net!("    - id: fw\n      type: firewall\n      params:\n        network: \"${net}\"\n        rules:\n          - {direction: out, port: \"443\", cidr: 0.0.0.0/0}\n"),
    ),
    case(
        "firewall",
        "icmp and any protocol",
        Same,
        with_net!("    - id: fw\n      type: firewall\n      params:\n        network: \"${net}\"\n        rules:\n          - {protocol: icmp, cidr: 10.0.0.0/8}\n          - {protocol: any, cidr: 10.1.0.0/16}\n"),
    ),
    case(
        "firewall",
        "no rules",
        Varies,
        with_net!("    - id: fw\n      type: firewall\n      params: {network: \"${net}\"}\n"),
    ),
    case(
        "firewall",
        "a rule with no cidr",
        Same,
        with_net!("    - id: fw\n      type: firewall\n      params:\n        network: \"${net}\"\n        rules: [{port: \"80\"}]\n"),
    ),
    case(
        "firewall",
        "a port range backwards",
        Same,
        with_net!("    - id: fw\n      type: firewall\n      params:\n        network: \"${net}\"\n        rules: [{port: 9000-8000, cidr: 0.0.0.0/0}]\n"),
    ),
    case(
        "firewall",
        "a direction no cloud has (`both`)",
        Same,
        with_net!("    - id: fw\n      type: firewall\n      params:\n        network: \"${net}\"\n        rules: [{direction: both, cidr: 0.0.0.0/0}]\n"),
    ),
    case(
        "firewall",
        "an icmp rule with a port",
        Same,
        with_net!("    - id: fw\n      type: firewall\n      params:\n        network: \"${net}\"\n        rules: [{protocol: icmp, port: \"8\", cidr: 0.0.0.0/0}]\n"),
    ),
    // ---- address -------------------------------------------------------
    case("address", "no params", Same, "    - id: ip\n      type: address\n"),
    case(
        "address",
        "a name",
        Varies,
        "    - id: ip\n      type: address\n      params: {name: web-ip}\n",
    ),
    case(
        "address",
        "tags",
        Same,
        "    - id: ip\n      type: address\n      params: {tags: {team: web}}\n",
    ),
    // ---- vm ------------------------------------------------------------
    case(
        "vm",
        "size and image by name, with a key",
        Same,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 2, memoryGb: 4}\n        image: {family: ubuntu, version: \"24.04\"}\n        sshKey: ssh-ed25519 AAAAexample\n"),
    ),
    case(
        "vm",
        "the cloud's own size and image as text",
        Same,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: Standard_B2s\n        image: my-image\n        sshKey: ssh-ed25519 AAAAexample\n"),
    ),
    case(
        "vm",
        "an address and a firewall",
        Same,
        with_subnet!("    - id: fw\n      type: firewall\n      params: {network: \"${net}\", rules: [{port: \"22\", cidr: 0.0.0.0/0}]}\n    - id: ip\n      type: address\n    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        address: \"${ip}\"\n        firewalls: [\"${fw}\"]\n        size: {cpus: 1, memoryGb: 1}\n        image: {family: debian, version: \"12\"}\n        sshKey: ssh-ed25519 AAAAexample\n"),
    ),
    case(
        "vm",
        "two firewalls",
        Varies,
        with_subnet!("    - id: a\n      type: firewall\n      params: {network: \"${net}\", rules: [{port: \"22\", cidr: 0.0.0.0/0}]}\n    - id: b\n      type: firewall\n      params: {network: \"${net}\", rules: [{port: \"80\", cidr: 0.0.0.0/0}]}\n    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        firewalls: [\"${a}\", \"${b}\"]\n        size: {cpus: 1, memoryGb: 1}\n        image: {family: debian, version: \"12\"}\n        sshKey: ssh-ed25519 AAAAexample\n"),
    ),
    case(
        "vm",
        "no ssh key",
        Varies,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 1, memoryGb: 1}\n        image: {family: debian, version: \"12\"}\n"),
    ),
    case(
        "vm",
        "a user",
        Varies,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 1, memoryGb: 1}\n        image: {family: debian, version: \"12\"}\n        sshKey: ssh-ed25519 AAAAexample\n        user: admin\n"),
    ),
    case(
        "vm",
        "bigger than any size in the table",
        Same,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 512, memoryGb: 4096}\n        image: {family: debian, version: \"12\"}\n        sshKey: ssh-ed25519 AAAAexample\n"),
    ),
    case(
        "vm",
        "an image family the vocabulary lacks (`windows`)",
        Same,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 1, memoryGb: 1}\n        image: {family: windows, version: \"11\"}\n        sshKey: ssh-ed25519 AAAAexample\n"),
    ),
    case(
        "vm",
        "a size with no memory",
        Same,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 2}\n        image: my-image\n        sshKey: ssh-ed25519 AAAAexample\n"),
    ),
    case(
        "vm",
        "no subnet",
        Same,
        "    - id: vm\n      type: vm\n      params:\n        size: t3.micro\n        image: my-image\n        sshKey: ssh-ed25519 AAAAexample\n",
    ),
    // ---- outputs and mixing --------------------------------------------
    case(
        "reads",
        "`publicAddress` of a vm with an address",
        Same,
        with_subnet!("    - id: ip\n      type: address\n    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        address: \"${ip}\"\n        size: {cpus: 1, memoryGb: 1}\n        image: my-image\n        sshKey: ssh-ed25519 AAAAexample\n  outputs:\n    - name: url\n      value: http://${vm.publicAddress}/\n"),
    ),
    case(
        "reads",
        "`publicAddress` of a vm with no address",
        Varies,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 1, memoryGb: 1}\n        image: my-image\n        sshKey: ssh-ed25519 AAAAexample\n  outputs:\n    - name: url\n      value: http://${vm.publicAddress}/\n"),
    ),
    case(
        "reads",
        "`privateAddress` and `id` of a vm",
        Same,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 1, memoryGb: 1}\n        image: my-image\n        sshKey: ssh-ed25519 AAAAexample\n  outputs:\n    - name: private\n      value: ${vm.privateAddress}\n    - name: id\n      value: ${vm.id}\n"),
    ),
    case(
        "reads",
        "`address` of an address",
        Same,
        "    - id: ip\n      type: address\n  outputs:\n    - name: public_ip\n      value: ${ip.address}\n",
    ),
    case(
        "reads",
        "an id taken by what a common type adds (`vm_nic`)",
        Varies,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 1, memoryGb: 1}\n        image: my-image\n        sshKey: ssh-ed25519 AAAAexample\n    - id: vm_nic\n      type: network\n      params: {cidr: 10.9.0.0/16}\n"),
    ),
    case(
        "disk",
        "two disks on a vm",
        Same,
        with_subnet!("    - id: vm\n      type: vm\n      params:\n        subnet: \"${sub}\"\n        size: {cpus: 1, memoryGb: 1}\n        image: {family: debian, version: \"12\"}\n        sshKey: ssh-ed25519 AAAAexample\n    - id: store\n      type: disk\n      params: {vm: \"${vm}\", sizeGb: 50}\n    - id: logs\n      type: disk\n      params: {vm: \"${vm}\", sizeGb: 20, tags: {role: logs}}\n"),
    ),
    case(
        "disk",
        "no vm and no location",
        Varies,
        "    - id: store\n      type: disk\n      params: {sizeGb: 50}\n",
    ),
    case(
        "disk",
        "a size of zero",
        Same,
        "    - id: store\n      type: disk\n      params: {sizeGb: 0, location: eu-west-1a}\n",
    ),
    case(
        "disk",
        "a vm that is not a reference",
        Same,
        "    - id: store\n      type: disk\n      params: {sizeGb: 10, vm: web}\n",
    ),
];

fn document(column: &Column, name: &str, resources: &str) -> String {
    // A case's outputs, when it has any, follow its resources.
    format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: {name}\nspec:\n  target: {}\n{}  resources:\n{}{resources}",
        column.target, column.settings, column.preamble
    )
}

fn compile(column: &Column, resources: &str) -> Result<Emitted, CompileError> {
    let graph = graph_from_text(
        "case.yaml",
        &document(column, "web", resources),
        column.target,
    );
    let target = emitter(column.target);
    check_settings(target.as_ref(), &graph)?;
    target.emit(&graph, &no_data())
}

const INTRODUCTION: &str = "\
# Conformance matrix for the common infrastructure types

What happens to the same document on each target, and each cloud, that maps
the [common infrastructure types](dsl/common-infrastructure-types.md)
(`network`, `subnet`, `firewall`, `address`, `vm`, `disk`). This page is generated:
`crates/meridian-cli/tests/infra_conformance.rs` compiles one small document
per row for every column and writes down how each compile came out, and it
fails when this page is out of date. So the page cannot drift from the code,
and a target cannot start accepting or refusing something without the change
showing here.

How to read a cell:

- **ok**: the document compiled.
- **ok, warns**: it compiled, with a warning the plan shows.
- a name such as `unsupported-param`: the target refused the document, and that
  is the rule the refusal carries, one of the [refusal rules](refusal-rules.md).
  The message names the resource, the line, and what the cloud does instead.

The **same** column says whether the columns are allowed to differ. A row
marked *yes* is a part of the vocabulary that must come out the same on every
cloud, and the test fails if one column breaks ranks. A row marked *no* is a
place where the clouds themselves differ; the cells say how, and
[the vocabulary's page](dsl/common-infrastructure-types.md) says why.

The configuration management types have [their own matrix](conformance-matrix.md).

";

fn headings() -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = Vec::new();
    for case in CASES {
        if !groups.contains(&case.group) {
            groups.push(case.group);
        }
    }
    groups
}

fn matrix() -> String {
    let mut out = INTRODUCTION.to_owned();
    for group in headings() {
        let _ = writeln!(out, "## `{group}`\n");
        let _ = write!(out, "| Document | same |");
        for column in COLUMNS {
            let _ = write!(out, " {} |", column.title);
        }
        let _ = write!(out, "\n|---|---|");
        for _ in COLUMNS {
            out.push_str("---|");
        }
        out.push('\n');
        for case in CASES.iter().filter(|c| c.group == group) {
            let same = if case.agreement == Same { "yes" } else { "no" };
            let _ = write!(out, "| {} | {same} |", case.name.replace('|', "\\|"));
            for column in COLUMNS {
                let _ = write!(out, " {} |", outcome(&compile(column, case.resources)));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_documented_matrix_is_what_the_targets_do() {
    let path = repo_root().join(PATH);
    let generated = matrix();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, &generated).unwrap();
    }
    let documented = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        documented == generated,
        "{PATH} is out of date with what the targets do; run the tests with UPDATE_GOLDEN=1 to record it again, and read the difference"
    );
}

#[test]
fn a_case_marked_same_comes_out_the_same_in_every_column() {
    let mut failures = Vec::new();
    for case in CASES.iter().filter(|c| c.agreement == Same) {
        let outcomes: Vec<(&str, String)> = COLUMNS
            .iter()
            .map(|c| (c.title, outcome(&compile(c, case.resources))))
            .collect();
        let verdict = |o: &str| {
            if o.starts_with("ok") {
                "ok".to_owned()
            } else {
                o.to_owned()
            }
        };
        let first = verdict(&outcomes[0].1);
        if outcomes.iter().any(|(_, o)| verdict(o) != first) {
            failures.push(format!("{} / {}: {outcomes:?}", case.group, case.name));
        }
    }
    assert!(
        failures.is_empty(),
        "these cases must come out the same in every column:\n{}",
        failures.join("\n")
    );
}

#[test]
fn case_names_are_unique_within_a_group() {
    let mut seen = std::collections::BTreeSet::new();
    for case in CASES {
        assert!(
            seen.insert((case.group, case.name)),
            "{} / {}",
            case.group,
            case.name
        );
    }
}

/// A target that takes the common types has a column here, and one that does
/// not refuses them under one rule, so a new IaC target cannot take them
/// without its cells being written down.
#[test]
fn every_target_that_takes_the_common_types_has_a_column() {
    for target in emitters() {
        let types = target.supported_resource_types();
        let takes = meridian_sdk::infra::common_type_names()
            .iter()
            .all(|t| types.iter().any(|s| s == t));
        assert_eq!(
            takes,
            COMMON_TARGETS.contains(&target.name()),
            "{}: declare all the common types and add a column, or none",
            target.name()
        );
        if takes {
            assert!(COLUMNS.iter().any(|c| c.target == target.name()));
            for name in meridian_sdk::infra::common_type_names() {
                assert!(
                    target
                        .resource_schemas()
                        .iter()
                        .any(|s| s.r#type == name && !s.open),
                    "{}: {name} is described for editors",
                    target.name()
                );
            }
        }
    }
}

/// Every refusal names its target and its resource, as on the other matrix.
#[test]
fn every_refusal_names_its_target_and_its_resource() {
    for case in CASES {
        for column in COLUMNS {
            if let Err(e) = compile(column, case.resources) {
                assert_eq!(e.target, column.target, "{}: {}", column.title, case.name);
                if e.rule != "invalid-setting" && !e.msg.starts_with("output ") {
                    assert!(
                        !e.resource.is_empty(),
                        "{} / {}: {e:?}",
                        column.title,
                        case.name
                    );
                }
            }
        }
    }
}

/// Every case compiles to the same bytes twice.
#[test]
fn every_case_compiles_to_the_same_bytes_twice() {
    for case in CASES {
        for column in COLUMNS {
            if let Ok(first) = compile(column, case.resources) {
                assert_eq!(
                    first,
                    compile(column, case.resources).unwrap(),
                    "{}: {}",
                    column.title,
                    case.name
                );
            }
        }
    }
}

/// A secret ssh key reaches each tool as the tool's own secret reference,
/// never as its value.
#[test]
fn a_secret_key_is_never_written_as_its_value() {
    let resources = format!(
        "{NET}{SUBNET}    - id: vm\n      type: vm\n      params:\n        subnet: \"${{sub}}\"\n        size: t3.micro\n        image: my-image\n        sshKey: {KEY}\n"
    );
    for column in COLUMNS {
        let mut graph = graph_from_text(
            "case.yaml",
            &document(column, "web", &resources),
            column.target,
        );
        secret_in_param(&mut graph, "vm", "sshKey", "admin_key");
        let emitted = emitter(column.target)
            .emit(&graph, &no_data())
            .unwrap_or_else(|e| panic!("{}: {e}", column.title));
        let all: String = emitted.artifact.files.values().cloned().collect();
        // CloudFormation names a parameter in letters and digits only.
        assert!(
            all.contains("meridian_secret_admin_key") || all.contains("MeridianSecretAdminKey"),
            "{}: {all}",
            column.title
        );
        assert!(!all.contains(KEY), "{}", column.title);
    }
}
