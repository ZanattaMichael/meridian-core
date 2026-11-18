//! Stage zero: the common infrastructure types (`network`, `subnet`,
//! `firewall`, `address`, `vm`) become Azure resource types, before the rest
//! of the compile sees the document.
//!
//! The vocabulary and its checks are the SDK's ([`meridian_sdk::infra`]);
//! this module holds only the Azure mapping. A resource is placed in the
//! deployment's resource group, at its `location` param or the group's own
//! location.

use std::collections::BTreeMap;

use meridian_sdk::infra::{
    self, asked_image, asked_size, closest_size, cloud_name, firewall_rules, helper, primary,
    referenced_id, Expansion,
};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{refuse, NAME};

/// The API version every mapped network and compute type is written at.
pub const NETWORK_API: &str = "2023-09-01";
/// The API version of virtual machines.
pub const COMPUTE_API: &str = "2024-03-01";

/// Azure sizes, smallest first: (size, processors, memory in GB). The same
/// table the Terraform target uses on Azure, so a document picks the same
/// machine with either tool.
pub const AZURE_SIZES: &[(&str, u32, f64)] = &[
    ("Standard_B1s", 1, 1.0),
    ("Standard_B1ms", 1, 2.0),
    ("Standard_B2s", 2, 4.0),
    ("Standard_B2ms", 2, 8.0),
    ("Standard_B4ms", 4, 16.0),
    ("Standard_B8ms", 8, 32.0),
    ("Standard_D16s_v5", 16, 64.0),
    ("Standard_D32s_v5", 32, 128.0),
];

/// The marketplace image of each image a vm asks for: publisher, offer, sku.
pub const AZURE_IMAGES: &[(&str, &str, &str, &str, &str)] = &[
    ("debian", "11", "Debian", "debian-11", "11-gen2"),
    ("debian", "12", "Debian", "debian-12", "12-gen2"),
    (
        "ubuntu",
        "22.04",
        "Canonical",
        "0001-com-ubuntu-server-jammy",
        "22_04-lts-gen2",
    ),
    ("ubuntu", "24.04", "Canonical", "ubuntu-24_04-lts", "server"),
];

/// The params of each common type this target takes.
pub fn params(name: &str) -> Vec<&'static str> {
    infra::common_params(name).iter().map(|(p, _)| *p).collect()
}

fn network_type(name: &str) -> String {
    format!("Microsoft.Network/{name}@{NETWORK_API}")
}

fn map(pairs: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn reference(id: &str, attribute: &str) -> Value {
    Value::String(format!("${{{id}.{attribute}}}"))
}

fn ref_param(r: &Resource, name: &str) -> Option<String> {
    r.params.get(name).and_then(referenced_id)
}

fn param(r: &Resource, name: &str) -> Value {
    r.params.get(name).cloned().unwrap_or(Value::Null)
}

fn location(r: &Resource) -> Value {
    r.params
        .get("location")
        .cloned()
        .unwrap_or_else(|| Value::from("${resourceGroup().location}"))
}

/// Name, location and tags: what every top-level Azure resource has.
fn top(r: &Resource, mut body: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    body.insert("name".to_owned(), cloud_name(r));
    body.insert("location".to_owned(), location(r));
    if let Some(tags) = r.params.get("tags") {
        body.insert("tags".to_owned(), tags.clone());
    }
    body
}

fn refuse_param(r: &Resource, name: &str, why: &str) -> CompileError {
    refuse(
        r,
        &rules::UNSUPPORTED_PARAM,
        format!(
            "param {name:?} of a {} cannot be expressed here: {why}",
            r.r#type
        ),
    )
}

fn outputs(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// Replaces the common resources of the tree with Azure's own.
pub(crate) fn lower(graph: &ResourceGraph) -> Result<ResourceGraph, CompileError> {
    let lowered = infra::lower(graph, |r, _| {
        infra::check_common(r).map_err(|e| CompileError {
            target: NAME.to_owned(),
            ..e
        })?;
        expand(r)
    })
    .map_err(|e| CompileError {
        target: NAME.to_owned(),
        ..e
    })?;
    let mut seen = std::collections::BTreeSet::new();
    for r in &lowered.resources {
        if !seen.insert(r.id.as_str()) {
            return Err(refuse(
                r,
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "resource id {:?} is taken twice: a common type adds a resource under it. \
                     Rename the document's own resource",
                    r.id
                ),
            ));
        }
    }
    Ok(lowered)
}

fn expand(r: &Resource) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    let own_id = || outputs(&[("id", format!("${{{id}.id}}"))]);
    Ok(match r.r#type.as_str() {
        "network" => Expansion {
            resources: vec![primary(
                r,
                &network_type("virtualNetworks"),
                top(
                    r,
                    map(vec![(
                        "properties",
                        Value::Map(map(vec![(
                            "addressSpace",
                            Value::Map(map(vec![(
                                "addressPrefixes",
                                Value::List(vec![param(r, "cidr")]),
                            )])),
                        )])),
                    )]),
                ),
            )],
            outputs: own_id(),
        },
        "subnet" => {
            if matches!(r.params.get("public"), Some(Value::Bool(true))) {
                return Err(refuse_param(
                    r,
                    "public",
                    "an Azure subnet gives no machine a public address of its own; attach an \
                     address resource to the vm instead",
                ));
            }
            if r.params.contains_key("tags") {
                return Err(refuse_param(r, "tags", "an Azure subnet carries no tags"));
            }
            if r.params.contains_key("location") {
                return Err(refuse_param(
                    r,
                    "location",
                    "an Azure subnet is wherever its network is; give the network the location",
                ));
            }
            let network = ref_param(r, "network").unwrap_or_default();
            Expansion {
                resources: vec![primary(
                    r,
                    &network_type("virtualNetworks/subnets"),
                    map(vec![
                        ("name", cloud_name(r)),
                        ("parent", Value::String(format!("${{{network}}}"))),
                        (
                            "properties",
                            Value::Map(map(vec![("addressPrefix", param(r, "cidr"))])),
                        ),
                    ]),
                )],
                outputs: own_id(),
            }
        }
        "firewall" => {
            // A location must be known when the deployment starts, so a
            // network's location cannot be read here; the resource group's
            // is used unless the firewall gives one.
            let mut body = top(r, map(vec![]));
            let rules: Vec<Value> = firewall_rules(r)
                .iter()
                .enumerate()
                .map(|(i, rule)| Value::Map(azure_rule(i, rule)))
                .collect();
            body.insert(
                "properties".to_owned(),
                Value::Map(map(vec![("securityRules", Value::List(rules))])),
            );
            Expansion {
                resources: vec![primary(r, &network_type("networkSecurityGroups"), body)],
                outputs: own_id(),
            }
        }
        "address" => Expansion {
            resources: vec![primary(
                r,
                &network_type("publicIPAddresses"),
                top(
                    r,
                    map(vec![
                        (
                            "sku",
                            Value::Map(map(vec![("name", Value::from("Standard"))])),
                        ),
                        (
                            "properties",
                            Value::Map(map(vec![(
                                "publicIPAllocationMethod",
                                Value::from("Static"),
                            )])),
                        ),
                    ]),
                ),
            )],
            outputs: outputs(&[
                ("address", format!("${{{id}.properties.ipAddress}}")),
                ("id", format!("${{{id}.id}}")),
            ]),
        },
        _ => vm(r)?,
    })
}

/// One security rule. Priorities follow the order the document lists the
/// rules in, from 100 up.
fn azure_rule(i: usize, rule: &infra::FirewallRule) -> BTreeMap<String, Value> {
    let inbound = rule.direction != "out";
    let ports = match rule.ports {
        None => "*".to_owned(),
        Some((a, b)) if a == b => a.to_string(),
        Some((a, b)) => format!("{a}-{b}"),
    };
    let protocol = match rule.protocol.as_str() {
        "tcp" => "Tcp",
        "udp" => "Udp",
        "icmp" => "Icmp",
        _ => "*",
    };
    let (source, destination) = if inbound {
        (rule.cidr.as_str(), "*")
    } else {
        ("*", rule.cidr.as_str())
    };
    map(vec![
        ("name", Value::String(format!("rule-{i}"))),
        (
            "properties",
            Value::Map(map(vec![
                ("priority", Value::Int(100 + i as i64)),
                (
                    "direction",
                    Value::from(if inbound { "Inbound" } else { "Outbound" }),
                ),
                ("access", Value::from("Allow")),
                ("protocol", Value::from(protocol)),
                ("sourcePortRange", Value::from("*")),
                ("destinationPortRange", Value::String(ports)),
                ("sourceAddressPrefix", Value::from(source)),
                ("destinationAddressPrefix", Value::from(destination)),
            ])),
        ),
    ])
}

fn vm(r: &Resource) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    let Some(key) = r.params.get("sshKey").cloned() else {
        return Err(refuse(
            r,
            &rules::MISSING_PARAM,
            "a vm on Azure needs sshKey: an Azure Linux machine is created with password login \
             off, so a key is the only way in"
                .to_owned(),
        ));
    };
    let firewalls: Vec<String> = match r.params.get("firewalls") {
        Some(Value::List(items)) => items.iter().filter_map(referenced_id).collect(),
        _ => Vec::new(),
    };
    if firewalls.len() > 1 {
        return Err(refuse_param(
            r,
            "firewalls",
            "an Azure network interface takes one network security group; put the rules in one \
             firewall",
        ));
    }
    let user = r
        .params
        .get("user")
        .cloned()
        .unwrap_or_else(|| Value::from("azureuser"));
    let user_text = match &user {
        Value::String(u) => u.clone(),
        _ => "azureuser".to_owned(),
    };
    let name_text = match r.params.get("name") {
        Some(Value::String(n)) => n.clone(),
        _ => r.id.clone(),
    };

    let mut configuration = map(vec![
        ("privateIPAllocationMethod", Value::from("Dynamic")),
        (
            "subnet",
            Value::Map(map(vec![(
                "id",
                reference(&ref_param(r, "subnet").unwrap_or_default(), "id"),
            )])),
        ),
    ]);
    let address = ref_param(r, "address");
    if let Some(address) = &address {
        configuration.insert(
            "publicIPAddress".to_owned(),
            Value::Map(map(vec![("id", reference(address, "id"))])),
        );
    }
    let mut nic_properties = map(vec![(
        "ipConfigurations",
        Value::List(vec![Value::Map(map(vec![
            ("name", Value::from("primary")),
            ("properties", Value::Map(configuration)),
        ]))]),
    )]);
    if let Some(firewall) = firewalls.first() {
        nic_properties.insert(
            "networkSecurityGroup".to_owned(),
            Value::Map(map(vec![("id", reference(firewall, "id"))])),
        );
    }
    let nic_id = format!("{id}_nic");
    let mut nic = map(vec![
        ("name", Value::String(format!("{name_text}-nic"))),
        ("location", location(r)),
        ("properties", Value::Map(nic_properties)),
    ]);
    if let Some(tags) = r.params.get("tags") {
        nic.insert("tags".to_owned(), tags.clone());
    }
    let nic = helper(r, "nic", &network_type("networkInterfaces"), nic);

    let size = param(r, "size");
    let size = match asked_size(&size) {
        Some((cpus, memory)) => {
            Value::from(closest_size(AZURE_SIZES, cpus, memory).ok_or_else(|| {
                let (largest, c, m) = AZURE_SIZES.last().expect("not empty");
                refuse(
                    r,
                    &rules::INVALID_PARAM,
                    format!(
                        "no size in the table has {cpus} processors and {memory} GB; the \
                         largest is {largest} ({c} processors, {m} GB). Write the cloud's own \
                         size as text for a larger machine"
                    ),
                )
            })?)
        }
        None => size,
    };
    let image = param(r, "image");
    let image_reference = match asked_image(&image) {
        Some((family, version)) => {
            let (_, _, publisher, offer, sku) = AZURE_IMAGES
                .iter()
                .find(|(f, v, _, _, _)| *f == family && *v == version)
                .expect("the SDK accepts only images every cloud maps");
            map(vec![
                ("publisher", Value::from(*publisher)),
                ("offer", Value::from(*offer)),
                ("sku", Value::from(*sku)),
                ("version", Value::from("latest")),
            ])
        }
        None => map(vec![("id", image)]),
    };
    let properties = map(vec![
        ("hardwareProfile", Value::Map(map(vec![("vmSize", size)]))),
        (
            "storageProfile",
            Value::Map(map(vec![
                ("imageReference", Value::Map(image_reference)),
                (
                    "osDisk",
                    Value::Map(map(vec![("createOption", Value::from("FromImage"))])),
                ),
            ])),
        ),
        (
            "osProfile",
            Value::Map(map(vec![
                ("computerName", Value::String(name_text)),
                ("adminUsername", user),
                (
                    "linuxConfiguration",
                    Value::Map(map(vec![
                        ("disablePasswordAuthentication", Value::Bool(true)),
                        (
                            "ssh",
                            Value::Map(map(vec![(
                                "publicKeys",
                                Value::List(vec![Value::Map(map(vec![
                                    (
                                        "path",
                                        Value::String(format!(
                                            "/home/{user_text}/.ssh/authorized_keys"
                                        )),
                                    ),
                                    ("keyData", key),
                                ]))]),
                            )])),
                        ),
                    ])),
                ),
            ])),
        ),
        (
            "networkProfile",
            Value::Map(map(vec![(
                "networkInterfaces",
                Value::List(vec![Value::Map(map(vec![(
                    "id",
                    reference(&nic_id, "id"),
                )]))]),
            )])),
        ),
    ]);
    let machine = primary(
        r,
        &format!("Microsoft.Compute/virtualMachines@{COMPUTE_API}"),
        top(r, map(vec![("properties", Value::Map(properties))])),
    );
    let mut reads = vec![
        ("id", format!("${{{id}.id}}")),
        (
            "privateAddress",
            format!("${{{nic_id}.properties.ipConfigurations[0].properties.privateIPAddress}}"),
        ),
    ];
    if let Some(address) = &address {
        reads.push((
            "publicAddress",
            format!("${{{address}.properties.ipAddress}}"),
        ));
    }
    Ok(Expansion {
        resources: vec![nic, machine],
        outputs: outputs(&reads),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_image_the_sdk_accepts_is_mapped() {
        for (family, versions) in infra::IMAGE_FAMILIES {
            for version in *versions {
                assert!(
                    AZURE_IMAGES
                        .iter()
                        .any(|(f, v, _, _, _)| f == family && v == version),
                    "{family} {version}"
                );
            }
        }
    }
}
