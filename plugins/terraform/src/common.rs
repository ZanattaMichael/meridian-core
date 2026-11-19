//! Stage zero: the common infrastructure types (`network`, `subnet`,
//! `firewall`, `address`, `vm`) become the resources of the AWS or Azure
//! provider, before the rest of the compile sees the document.
//!
//! The vocabulary and its checks are the SDK's ([`meridian_sdk::infra`]);
//! this module holds only the Terraform mapping for each cloud. Which cloud
//! a document is aimed at is `spec.settings.cloud`, or, when that is not
//! given, the one cloud whose provider the document configures.

use std::collections::BTreeMap;

use meridian_sdk::infra::{
    self, asked_image, asked_size, closest_size, cloud_name, firewall_rules, helper, primary,
    referenced_id, Expansion,
};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{refuse, refuse_at, NAME};

/// The keys this target reads in `spec.settings`.
pub const SETTINGS_KEYS: [&str; 3] = ["cloud", "location", "resourceGroup"];

/// The clouds the common types map to on this target.
pub const CLOUDS: &[&str] = &["aws", "azure", "gcp"];

/// The provider each cloud is configured with.
fn provider_of(cloud: &str) -> &'static str {
    match cloud {
        "aws" => "aws",
        "gcp" => "google",
        _ => "azurerm",
    }
}

/// AWS sizes, smallest first: (instance type, processors, memory in GB).
pub const AWS_SIZES: &[(&str, u32, f64)] = &[
    ("t3.nano", 2, 0.5),
    ("t3.micro", 2, 1.0),
    ("t3.small", 2, 2.0),
    ("t3.medium", 2, 4.0),
    ("t3.large", 2, 8.0),
    ("t3.xlarge", 4, 16.0),
    ("t3.2xlarge", 8, 32.0),
    ("m6i.4xlarge", 16, 64.0),
    ("m6i.8xlarge", 32, 128.0),
];

/// Azure sizes, smallest first: (size, processors, memory in GB).
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

/// The AMI owner and name pattern of each image an AWS vm asks for by
/// family and version: the publisher's own account, so no other account's
/// look-alike is picked.
pub const AWS_IMAGES: &[(&str, &str, &str, &str)] = &[
    ("debian", "11", "136693071363", "debian-11-amd64-*"),
    ("debian", "12", "136693071363", "debian-12-amd64-*"),
    (
        "ubuntu",
        "22.04",
        "099720109477",
        "ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*",
    ),
    (
        "ubuntu",
        "24.04",
        "099720109477",
        "ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*",
    ),
];

/// The marketplace image of each Azure image: publisher, offer, sku.
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

/// The params of each common type this target takes, on any cloud. A param
/// one cloud cannot express is refused by that cloud's mapping with the
/// reason.
pub fn params(name: &str) -> Vec<&'static str> {
    infra::common_params(name).iter().map(|(p, _)| *p).collect()
}

/// Which cloud the document's common resources map to.
fn cloud(graph: &ResourceGraph, first: &Resource) -> Result<String, CompileError> {
    if let Some(value) = graph.settings.get("cloud") {
        return match value.as_str() {
            Some(c) if CLOUDS.contains(&c) => Ok(c.to_owned()),
            _ => Err(setting_error(format!(
                "spec.settings.cloud must be one of {}, not {}",
                CLOUDS.join(", "),
                value.canonical()
            ))),
        };
    }
    let configured: Vec<&str> = CLOUDS
        .iter()
        .copied()
        .filter(|c| {
            graph.resources.iter().any(|r| {
                r.r#type == "provider"
                    && match r.params.get("name") {
                        Some(Value::String(n)) => n == provider_of(c),
                        _ => r.id == provider_of(c),
                    }
            })
        })
        .collect();
    match configured.as_slice() {
        [one] => Ok((*one).to_owned()),
        _ => Err(refuse(
            first,
            &rules::MISSING_RESOURCE,
            format!(
                "a {} is mapped to one cloud's resources, and this document {}. Say which in \
                 spec.settings.cloud ({})",
                first.r#type,
                if configured.is_empty() {
                    "configures no aws, azurerm or google provider".to_owned()
                } else {
                    "configures more than one of the aws, azurerm and google providers".to_owned()
                },
                CLOUDS.join(", ")
            ),
        )),
    }
}

fn setting_error(msg: String) -> CompileError {
    refuse_at("", &Default::default(), &rules::INVALID_SETTING, msg)
}

fn text_setting(graph: &ResourceGraph, key: &str) -> Result<Option<Value>, CompileError> {
    match graph.settings.get(key) {
        None => Ok(None),
        Some(v @ Value::String(_)) => Ok(Some(v.clone())),
        Some(other) => Err(setting_error(format!(
            "spec.settings.{key} must be text, not {}",
            other.describe()
        ))),
    }
}

/// Replaces the common resources of the tree with the cloud's own.
pub(crate) fn lower(graph: &ResourceGraph) -> Result<ResourceGraph, CompileError> {
    let mut state = State::default();
    let mut chosen: Option<String> = None;
    let lowered = infra::lower(graph, |r, graph| {
        infra::check_common(r).map_err(|e| CompileError {
            target: NAME.to_owned(),
            ..e
        })?;
        let cloud = match &chosen {
            Some(c) => c.clone(),
            None => {
                let c = cloud(graph, r)?;
                chosen = Some(c.clone());
                c
            }
        };
        match cloud.as_str() {
            "aws" => aws(r, graph, &mut state),
            "gcp" => gcp(r, graph),
            _ => azure(r, graph),
        }
    })
    .map_err(|e| CompileError {
        target: NAME.to_owned(),
        ..e
    })?;
    if chosen.is_none() {
        for key in ["location", "resourceGroup"] {
            if graph.settings.contains_key(key) {
                return Err(setting_error(format!(
                    "spec.settings.{key} is read only for the common infrastructure types, and \
                     this document has none"
                )));
            }
        }
    }
    check_unique(&lowered)?;
    Ok(lowered)
}

/// A helper an expansion adds must not take an id the document uses.
fn check_unique(graph: &ResourceGraph) -> Result<(), CompileError> {
    let mut seen = std::collections::BTreeSet::new();
    for r in &graph.resources {
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
    Ok(())
}

/// What an AWS mapping must remember between resources: the internet
/// gateway a network's public subnets share.
#[derive(Default)]
struct State {
    gateways: std::collections::BTreeSet<String>,
}

fn map(pairs: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn reference(id: &str, attribute: &str) -> Value {
    Value::String(format!("${{{id}.{attribute}}}"))
}

fn param(r: &Resource, name: &str) -> Option<Value> {
    r.params.get(name).cloned()
}

fn ref_param(r: &Resource, name: &str) -> Option<String> {
    r.params.get(name).and_then(referenced_id)
}

/// Tags with the resource's name added under `Name`, the way the AWS
/// console shows a resource.
fn aws_tags(r: &Resource) -> Value {
    let mut tags = match r.params.get("tags") {
        Some(Value::Map(m)) => m.clone(),
        _ => BTreeMap::new(),
    };
    tags.entry("Name".to_owned())
        .or_insert_with(|| cloud_name(r));
    Value::Map(tags)
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

fn aws(r: &Resource, graph: &ResourceGraph, state: &mut State) -> Result<Expansion, CompileError> {
    if r.params.contains_key("location") && r.r#type != "subnet" && r.r#type != "disk" {
        return Err(refuse_param(
            r,
            "location",
            "on AWS a resource is in the region its provider is configured for; only a subnet \
             and a disk take a location, an availability zone",
        ));
    }
    let id = r.id.as_str();
    Ok(match r.r#type.as_str() {
        "network" => Expansion {
            order: Vec::new(),
            resources: vec![primary(
                r,
                "aws_vpc",
                map(vec![
                    ("cidr_block", param(r, "cidr").unwrap_or(Value::Null)),
                    ("enable_dns_hostnames", Value::Bool(true)),
                    ("tags", aws_tags(r)),
                ]),
            )],
            outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
        },
        "subnet" => {
            let network = ref_param(r, "network").unwrap_or_default();
            let public = matches!(r.params.get("public"), Some(Value::Bool(true)));
            let mut body = map(vec![
                ("vpc_id", reference(&network, "id")),
                ("cidr_block", param(r, "cidr").unwrap_or(Value::Null)),
                ("map_public_ip_on_launch", Value::Bool(public)),
                ("tags", aws_tags(r)),
            ]);
            if let Some(zone) = param(r, "location") {
                body.insert("availability_zone".to_owned(), zone);
            }
            let mut resources = Vec::new();
            if public {
                // One internet gateway per network, which every public
                // subnet of it routes through.
                let gateway = format!("{network}_gateway");
                if state.gateways.insert(network.clone()) {
                    let network_resource = graph.resources.iter().find(|n| n.id == network);
                    let mut gw = helper(
                        network_resource.unwrap_or(r),
                        "gateway",
                        "aws_internet_gateway",
                        map(vec![("vpc_id", reference(&network, "id"))]),
                    );
                    gw.id = gateway.clone();
                    resources.push(gw);
                }
                resources.push(primary(r, "aws_subnet", body));
                // Routes are separate resources: the table's inline `route`
                // would have to list every attribute in Terraform's JSON.
                resources.push(helper(
                    r,
                    "routes",
                    "aws_route_table",
                    map(vec![
                        ("vpc_id", reference(&network, "id")),
                        ("tags", aws_tags(r)),
                    ]),
                ));
                resources.push(helper(
                    r,
                    "routes_internet",
                    "aws_route",
                    map(vec![
                        ("route_table_id", reference(&format!("{id}_routes"), "id")),
                        ("destination_cidr_block", Value::from("0.0.0.0/0")),
                        ("gateway_id", reference(&gateway, "id")),
                    ]),
                ));
                resources.push(helper(
                    r,
                    "routes_link",
                    "aws_route_table_association",
                    map(vec![
                        ("subnet_id", reference(id, "id")),
                        ("route_table_id", reference(&format!("{id}_routes"), "id")),
                    ]),
                ));
            } else {
                resources.push(primary(r, "aws_subnet", body));
            }
            Expansion {
                order: Vec::new(),
                resources,
                outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
            }
        }
        "firewall" => {
            let network = ref_param(r, "network").unwrap_or_default();
            let mut resources = vec![primary(
                r,
                "aws_security_group",
                map(vec![
                    ("name", cloud_name(r)),
                    ("vpc_id", reference(&network, "id")),
                    ("tags", aws_tags(r)),
                ]),
            )];
            let rules = firewall_rules(r);
            let mut any_out = false;
            for (i, rule) in rules.iter().enumerate() {
                let (kind, suffix) = if rule.direction == "out" {
                    any_out = true;
                    ("aws_vpc_security_group_egress_rule", format!("out_{i}"))
                } else {
                    ("aws_vpc_security_group_ingress_rule", format!("in_{i}"))
                };
                let mut body = map(vec![("security_group_id", reference(id, "id"))]);
                let cidr_key = if rule.cidr.contains(':') {
                    "cidr_ipv6"
                } else {
                    "cidr_ipv4"
                };
                body.insert(cidr_key.to_owned(), Value::from(rule.cidr.as_str()));
                match (rule.protocol.as_str(), rule.ports) {
                    ("any", _) => {
                        body.insert("ip_protocol".to_owned(), Value::from("-1"));
                    }
                    ("icmp", _) => {
                        body.insert("ip_protocol".to_owned(), Value::from("icmp"));
                        body.insert("from_port".to_owned(), Value::Int(-1));
                        body.insert("to_port".to_owned(), Value::Int(-1));
                    }
                    (protocol, ports) => {
                        let (from, to) = ports.unwrap_or((0, 65535));
                        body.insert("ip_protocol".to_owned(), Value::from(protocol));
                        body.insert("from_port".to_owned(), Value::Int(from.into()));
                        body.insert("to_port".to_owned(), Value::Int(to.into()));
                    }
                }
                resources.push(helper(r, &suffix, kind, body));
            }
            if !any_out {
                // Terraform removes the allow-all rule AWS gives a new
                // security group, so the cloud's default is written back.
                resources.push(helper(
                    r,
                    "out_all",
                    "aws_vpc_security_group_egress_rule",
                    map(vec![
                        ("security_group_id", reference(id, "id")),
                        ("cidr_ipv4", Value::from("0.0.0.0/0")),
                        ("ip_protocol", Value::from("-1")),
                    ]),
                ));
            }
            Expansion {
                order: Vec::new(),
                resources,
                outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
            }
        }
        "address" => {
            if r.params.contains_key("name") {
                return Err(refuse_param(
                    r,
                    "name",
                    "an AWS elastic IP has no name; it is shown by its Name tag, which is the \
                     resource's id unless tags.Name is given",
                ));
            }
            Expansion {
                order: Vec::new(),
                resources: vec![primary(
                    r,
                    "aws_eip",
                    map(vec![("domain", Value::from("vpc")), ("tags", aws_tags(r))]),
                )],
                outputs: outputs(&[
                    ("address", format!("${{{id}.public_ip}}")),
                    ("id", format!("${{{id}.id}}")),
                ]),
            }
        }
        "disk" => aws_disk(r, graph)?,
        _ => aws_vm(r)?,
    })
}

fn aws_vm(r: &Resource) -> Result<Expansion, CompileError> {
    if r.params.contains_key("user") {
        return Err(refuse_param(
            r,
            "user",
            "on AWS the image decides the login (ubuntu for Ubuntu, admin for Debian)",
        ));
    }
    let id = r.id.as_str();
    let mut resources = Vec::new();
    let mut body = map(vec![
        (
            "subnet_id",
            reference(&ref_param(r, "subnet").unwrap_or_default(), "id"),
        ),
        ("tags", aws_tags(r)),
    ]);
    let size = r.params.get("size").cloned().unwrap_or(Value::Null);
    let instance_type = match asked_size(&size) {
        Some((cpus, memory)) => Value::from(
            closest_size(AWS_SIZES, cpus, memory)
                .ok_or_else(|| too_big(r, cpus, memory, AWS_SIZES))?,
        ),
        None => size,
    };
    body.insert("instance_type".to_owned(), instance_type);
    let image = r.params.get("image").cloned().unwrap_or(Value::Null);
    match asked_image(&image) {
        Some((family, version)) => {
            let (_, _, owner, pattern) = AWS_IMAGES
                .iter()
                .find(|(f, v, _, _)| *f == family && *v == version)
                .expect("the SDK accepts only images every cloud maps");
            resources.push(helper(
                r,
                "image",
                "data.aws_ami",
                map(vec![
                    ("most_recent", Value::Bool(true)),
                    ("owners", Value::List(vec![Value::from(*owner)])),
                    (
                        "filter",
                        Value::List(vec![
                            Value::Map(map(vec![
                                ("name", Value::from("name")),
                                ("values", Value::List(vec![Value::from(*pattern)])),
                            ])),
                            Value::Map(map(vec![
                                ("name", Value::from("architecture")),
                                ("values", Value::List(vec![Value::from("x86_64")])),
                            ])),
                        ]),
                    ),
                ]),
            ));
            body.insert("ami".to_owned(), reference(&format!("{id}_image"), "id"));
        }
        None => {
            body.insert("ami".to_owned(), image);
        }
    }
    if let Some(Value::List(firewalls)) = r.params.get("firewalls") {
        body.insert(
            "vpc_security_group_ids".to_owned(),
            Value::List(
                firewalls
                    .iter()
                    .filter_map(referenced_id)
                    .map(|f| reference(&f, "id"))
                    .collect(),
            ),
        );
    }
    if let Some(key) = param(r, "sshKey") {
        resources.push(helper(
            r,
            "key",
            "aws_key_pair",
            map(vec![("key_name", cloud_name(r)), ("public_key", key)]),
        ));
        body.insert(
            "key_name".to_owned(),
            reference(&format!("{id}_key"), "key_name"),
        );
    }
    resources.push(primary(r, "aws_instance", body));
    let public = match ref_param(r, "address") {
        Some(address) => {
            resources.push(helper(
                r,
                "address",
                "aws_eip_association",
                map(vec![
                    ("instance_id", reference(id, "id")),
                    ("allocation_id", reference(&address, "id")),
                ]),
            ));
            format!("${{{address}.public_ip}}")
        }
        None => format!("${{{id}.public_ip}}"),
    };
    Ok(Expansion {
        order: Vec::new(),
        resources,
        outputs: outputs(&[
            ("id", format!("${{{id}.id}}")),
            ("privateAddress", format!("${{{id}.private_ip}}")),
            ("publicAddress", public),
        ]),
    })
}

/// A disk's place among the disks of its vm, which numbers its attachment.
fn disk_index(r: &Resource, graph: &ResourceGraph, vm: &str) -> usize {
    infra::disks_of(graph, vm)
        .iter()
        .position(|d| d.id == r.id)
        .unwrap_or(0)
}

/// The device names an AWS data disk is attached as, in order.
const AWS_DEVICES: &str = "fghijklmnop";

fn aws_disk(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    let vm = ref_param(r, "vm");
    let zone = match (param(r, "location"), &vm) {
        (Some(zone), _) => zone,
        (None, Some(vm)) => reference(vm, "availability_zone"),
        (None, None) => {
            return Err(refuse(
                r,
                &rules::MISSING_PARAM,
                "a disk on AWS is in an availability zone: give the param vm, or location \
                 (such as eu-west-1a)"
                    .to_owned(),
            ))
        }
    };
    let mut resources = vec![primary(
        r,
        "aws_ebs_volume",
        map(vec![
            ("availability_zone", zone),
            ("size", Value::Uint(infra::disk_size(r))),
            ("type", Value::from("gp3")),
            ("tags", aws_tags(r)),
        ]),
    )];
    if let Some(vm) = vm {
        let i = disk_index(r, graph, &vm);
        let letter = AWS_DEVICES.chars().nth(i).ok_or_else(|| {
            refuse(
                r,
                &rules::INVALID_PARAM,
                format!(
                    "vm {vm:?} has more than {} disks; AWS names them /dev/sdf to /dev/sdp",
                    AWS_DEVICES.len()
                ),
            )
        })?;
        resources.push(helper(
            r,
            "attachment",
            "aws_volume_attachment",
            map(vec![
                ("device_name", Value::String(format!("/dev/sd{letter}"))),
                ("volume_id", reference(id, "id")),
                ("instance_id", reference(&vm, "id")),
            ]),
        ));
    }
    Ok(Expansion {
        order: Vec::new(),
        resources,
        outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
    })
}

fn azure_disk(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    let mut body = with(
        map(vec![
            ("name", cloud_name(r)),
            ("storage_account_type", Value::from("StandardSSD_LRS")),
            ("create_option", Value::from("Empty")),
            ("disk_size_gb", Value::Uint(infra::disk_size(r))),
        ]),
        azure_place(r, graph, Some(azure_location(r, graph)?))?,
    );
    azure_tags(r, &mut body);
    let mut resources = vec![primary(r, "azurerm_managed_disk", body)];
    if let Some(vm) = ref_param(r, "vm") {
        resources.push(helper(
            r,
            "attachment",
            "azurerm_virtual_machine_data_disk_attachment",
            map(vec![
                ("managed_disk_id", reference(id, "id")),
                ("virtual_machine_id", reference(&vm, "id")),
                ("lun", Value::Uint(disk_index(r, graph, &vm) as u64)),
                ("caching", Value::from("ReadWrite")),
            ]),
        ));
    }
    Ok(Expansion {
        order: Vec::new(),
        resources,
        outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
    })
}

fn too_big(r: &Resource, cpus: u32, memory: f64, table: &[(&str, u32, f64)]) -> CompileError {
    let (largest, c, m) = table.last().expect("a size table is not empty");
    refuse(
        r,
        &rules::INVALID_PARAM,
        format!(
            "no size in the table has {cpus} processors and {memory} GB; the largest is \
             {largest} ({c} processors, {m} GB). Write the cloud's own size as text for a \
             larger machine"
        ),
    )
}

/// The Azure location and resource group every Azure resource carries.
fn azure_place(
    r: &Resource,
    graph: &ResourceGraph,
    location: Option<Value>,
) -> Result<Vec<(&'static str, Value)>, CompileError> {
    let group = text_setting(graph, "resourceGroup")?.ok_or_else(|| {
        refuse(
            r,
            &rules::MISSING_RESOURCE,
            format!(
                "a {} on Azure is created in a resource group: name it in \
                 spec.settings.resourceGroup",
                r.r#type
            ),
        )
    })?;
    let mut place = vec![("resource_group_name", group)];
    if let Some(location) = location {
        place.push(("location", location));
    }
    Ok(place)
}

fn azure_location(r: &Resource, graph: &ResourceGraph) -> Result<Value, CompileError> {
    if let Some(location) = param(r, "location") {
        return Ok(location);
    }
    text_setting(graph, "location")?.ok_or_else(|| {
        refuse(
            r,
            &rules::MISSING_PARAM,
            format!(
                "a {} on Azure needs a location: give the param location, or \
                 spec.settings.location for the whole document",
                r.r#type
            ),
        )
    })
}

fn with(mut body: BTreeMap<String, Value>, more: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    for (k, v) in more {
        body.insert(k.to_owned(), v);
    }
    body
}

fn azure_tags(r: &Resource, body: &mut BTreeMap<String, Value>) {
    if let Some(tags) = param(r, "tags") {
        body.insert("tags".to_owned(), tags);
    }
}

fn azure(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    Ok(match r.r#type.as_str() {
        "network" => {
            let mut body = with(
                map(vec![
                    ("name", cloud_name(r)),
                    (
                        "address_space",
                        Value::List(vec![param(r, "cidr").unwrap_or(Value::Null)]),
                    ),
                ]),
                azure_place(r, graph, Some(azure_location(r, graph)?))?,
            );
            azure_tags(r, &mut body);
            Expansion {
                order: Vec::new(),
                resources: vec![primary(r, "azurerm_virtual_network", body)],
                outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
            }
        }
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
            let body = with(
                map(vec![
                    ("name", cloud_name(r)),
                    ("virtual_network_name", reference(&network, "name")),
                    (
                        "address_prefixes",
                        Value::List(vec![param(r, "cidr").unwrap_or(Value::Null)]),
                    ),
                ]),
                azure_place(r, graph, None)?,
            );
            Expansion {
                order: Vec::new(),
                resources: vec![primary(r, "azurerm_subnet", body)],
                outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
            }
        }
        "firewall" => {
            // A security group is not part of a network on Azure; it is
            // placed where the network is, unless told otherwise.
            let location = match param(r, "location") {
                Some(l) => l,
                None => reference(&ref_param(r, "network").unwrap_or_default(), "location"),
            };
            let mut body = with(
                map(vec![("name", cloud_name(r))]),
                azure_place(r, graph, Some(location))?,
            );
            azure_tags(r, &mut body);
            let mut resources = vec![primary(r, "azurerm_network_security_group", body)];
            // Each rule is a resource of its own: the group's inline
            // `security_rule` would have to list every attribute in
            // Terraform's JSON.
            for (i, rule) in firewall_rules(r).iter().enumerate() {
                let body = with(
                    azure_rule(i, rule),
                    vec![
                        ("network_security_group_name", reference(id, "name")),
                        (
                            "resource_group_name",
                            text_setting(graph, "resourceGroup")?.unwrap_or(Value::Null),
                        ),
                    ],
                );
                resources.push(helper(
                    r,
                    &format!("rule_{i}"),
                    "azurerm_network_security_rule",
                    body,
                ));
            }
            Expansion {
                order: Vec::new(),
                resources,
                outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
            }
        }
        "address" => {
            let mut body = with(
                map(vec![
                    ("name", cloud_name(r)),
                    ("allocation_method", Value::from("Static")),
                    ("sku", Value::from("Standard")),
                ]),
                azure_place(r, graph, Some(azure_location(r, graph)?))?,
            );
            azure_tags(r, &mut body);
            Expansion {
                order: Vec::new(),
                resources: vec![primary(r, "azurerm_public_ip", body)],
                outputs: outputs(&[
                    ("address", format!("${{{id}.ip_address}}")),
                    ("id", format!("${{{id}.id}}")),
                ]),
            }
        }
        "disk" => azure_disk(r, graph)?,
        _ => azure_vm(r, graph)?,
    })
}

/// One security rule of an Azure network security group. Priorities are
/// given in the order the document lists the rules, from 100 up.
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
        ("priority", Value::Int(100 + i as i64)),
        (
            "direction",
            Value::from(if inbound { "Inbound" } else { "Outbound" }),
        ),
        ("access", Value::from("Allow")),
        ("protocol", Value::from(protocol)),
        ("source_port_range", Value::from("*")),
        ("destination_port_range", Value::String(ports)),
        ("source_address_prefix", Value::from(source)),
        ("destination_address_prefix", Value::from(destination)),
    ])
}

fn azure_vm(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    let Some(key) = param(r, "sshKey") else {
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
    let location = azure_location(r, graph)?;
    let user = param(r, "user").unwrap_or_else(|| Value::from("azureuser"));
    let mut resources = Vec::new();

    let mut configuration = map(vec![
        ("name", Value::from("primary")),
        (
            "subnet_id",
            reference(&ref_param(r, "subnet").unwrap_or_default(), "id"),
        ),
        ("private_ip_address_allocation", Value::from("Dynamic")),
    ]);
    if let Some(address) = ref_param(r, "address") {
        configuration.insert("public_ip_address_id".to_owned(), reference(&address, "id"));
    }
    let nic_id = format!("{id}_nic");
    resources.push(helper(
        r,
        "nic",
        "azurerm_network_interface",
        with(
            map(vec![
                ("name", Value::String(format!("{}-nic", name_text(r)))),
                (
                    "ip_configuration",
                    Value::List(vec![Value::Map(configuration)]),
                ),
            ]),
            azure_place(r, graph, Some(location.clone()))?,
        ),
    ));
    if let Some(firewall) = firewalls.first() {
        resources.push(helper(
            r,
            "nic_firewall",
            "azurerm_network_interface_security_group_association",
            map(vec![
                ("network_interface_id", reference(&nic_id, "id")),
                ("network_security_group_id", reference(firewall, "id")),
            ]),
        ));
    }

    let size = r.params.get("size").cloned().unwrap_or(Value::Null);
    let size = match asked_size(&size) {
        Some((cpus, memory)) => Value::from(
            closest_size(AZURE_SIZES, cpus, memory)
                .ok_or_else(|| too_big(r, cpus, memory, AZURE_SIZES))?,
        ),
        None => size,
    };
    let mut body = with(
        map(vec![
            ("name", cloud_name(r)),
            ("size", size),
            ("admin_username", user.clone()),
            (
                "network_interface_ids",
                Value::List(vec![reference(&nic_id, "id")]),
            ),
            (
                "admin_ssh_key",
                Value::List(vec![Value::Map(map(vec![
                    ("username", user),
                    ("public_key", key),
                ]))]),
            ),
            (
                "os_disk",
                Value::Map(map(vec![
                    ("caching", Value::from("ReadWrite")),
                    ("storage_account_type", Value::from("Standard_LRS")),
                ])),
            ),
        ]),
        azure_place(r, graph, Some(location))?,
    );
    let image = r.params.get("image").cloned().unwrap_or(Value::Null);
    match asked_image(&image) {
        Some((family, version)) => {
            let (_, _, publisher, offer, sku) = AZURE_IMAGES
                .iter()
                .find(|(f, v, _, _, _)| *f == family && *v == version)
                .expect("the SDK accepts only images every cloud maps");
            body.insert(
                "source_image_reference".to_owned(),
                Value::Map(map(vec![
                    ("publisher", Value::from(*publisher)),
                    ("offer", Value::from(*offer)),
                    ("sku", Value::from(*sku)),
                    ("version", Value::from("latest")),
                ])),
            );
        }
        None => {
            body.insert("source_image_id".to_owned(), image);
        }
    }
    azure_tags(r, &mut body);
    resources.push(primary(r, "azurerm_linux_virtual_machine", body));
    let mut reads = vec![
        ("id", format!("${{{id}.id}}")),
        ("privateAddress", format!("${{{id}.private_ip_address}}")),
    ];
    if let Some(address) = ref_param(r, "address") {
        reads.push(("publicAddress", format!("${{{address}.ip_address}}")));
    }
    Ok(Expansion {
        order: Vec::new(),
        resources,
        outputs: outputs(&reads),
    })
}

/// The cloud name as text, for names derived from it; a name that is not
/// plain text falls back to the id.
fn name_text(r: &Resource) -> String {
    match r.params.get("name") {
        Some(Value::String(n)) => n.clone(),
        _ => r.id.clone(),
    }
}

// ---- Google Cloud ----

/// Google Cloud machine types, smallest first: (machine type, processors,
/// memory in GB).
pub const GCP_SIZES: &[(&str, u32, f64)] = &[
    ("e2-micro", 2, 1.0),
    ("e2-small", 2, 2.0),
    ("e2-medium", 2, 4.0),
    ("e2-standard-2", 2, 8.0),
    ("e2-standard-4", 4, 16.0),
    ("e2-standard-8", 8, 32.0),
    ("e2-standard-16", 16, 64.0),
    ("e2-standard-32", 32, 128.0),
];

/// The public image family of each image a vm asks for, as
/// `<project>/<family>`: the newest image of the family is used.
pub const GCP_IMAGES: &[(&str, &str, &str)] = &[
    ("debian", "11", "debian-cloud/debian-11"),
    ("debian", "12", "debian-cloud/debian-12"),
    ("ubuntu", "22.04", "ubuntu-os-cloud/ubuntu-2204-lts"),
    ("ubuntu", "24.04", "ubuntu-os-cloud/ubuntu-2404-lts-amd64"),
];

/// The region a regional resource is in: its `location`, or
/// `spec.settings.location`.
fn gcp_region(r: &Resource, graph: &ResourceGraph) -> Result<Value, CompileError> {
    if let Some(location) = param(r, "location") {
        return Ok(location);
    }
    text_setting(graph, "location")?.ok_or_else(|| {
        refuse(
            r,
            &rules::MISSING_PARAM,
            format!(
                "a {} on Google Cloud is in a region: give the param location, or \
                 spec.settings.location for the whole document",
                r.r#type
            ),
        )
    })
}

/// A Google Cloud name: lower case letters, digits and hyphens, so an
/// underscore of the id becomes a hyphen. A name given as text is used as
/// written, and the cloud says if it is not one.
fn gcp_name(r: &Resource) -> Value {
    match r.params.get("name") {
        Some(name) => name.clone(),
        None => Value::String(r.id.replace('_', "-")),
    }
}

fn gcp_labels(r: &Resource, body: &mut BTreeMap<String, Value>) {
    if let Some(tags) = param(r, "tags") {
        body.insert("labels".to_owned(), tags);
    }
}

/// The network tag a firewall's rules apply to, and that a vm the firewall
/// protects carries.
fn gcp_target_tag(id: &str) -> Value {
    Value::String(format!("meridian-{}", id.replace('_', "-")))
}

fn gcp(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
    if graph.settings.contains_key("resourceGroup") {
        return Err(setting_error(
            "spec.settings.resourceGroup is Azure's; on Google Cloud the google provider's \
             project decides where resources go"
                .to_owned(),
        ));
    }
    let id = r.id.as_str();
    let own_id = || outputs(&[("id", format!("${{{id}.id}}"))]);
    Ok(match r.r#type.as_str() {
        "network" => {
            if r.params.contains_key("location") {
                return Err(refuse_param(
                    r,
                    "location",
                    "a Google Cloud network is global; give its subnets a region",
                ));
            }
            if r.params.contains_key("tags") {
                return Err(refuse_param(
                    r,
                    "tags",
                    "a Google Cloud network carries no labels",
                ));
            }
            Expansion {
                order: Vec::new(),
                resources: vec![primary(
                    r,
                    "google_compute_network",
                    map(vec![
                        ("name", gcp_name(r)),
                        ("auto_create_subnetworks", Value::Bool(false)),
                        // No range of its own: the cidr is kept as the
                        // description, so the document's value is visible.
                        ("description", param(r, "cidr").unwrap_or(Value::Null)),
                    ]),
                )],
                outputs: own_id(),
            }
        }
        "subnet" => {
            if matches!(r.params.get("public"), Some(Value::Bool(true))) {
                return Err(refuse_param(
                    r,
                    "public",
                    "a Google Cloud subnet gives no machine a public address of its own; attach \
                     an address resource to the vm instead",
                ));
            }
            if r.params.contains_key("tags") {
                return Err(refuse_param(
                    r,
                    "tags",
                    "a Google Cloud subnet carries no labels",
                ));
            }
            let network = ref_param(r, "network").unwrap_or_default();
            Expansion {
                order: Vec::new(),
                resources: vec![primary(
                    r,
                    "google_compute_subnetwork",
                    map(vec![
                        ("name", gcp_name(r)),
                        ("network", reference(&network, "id")),
                        ("ip_cidr_range", param(r, "cidr").unwrap_or(Value::Null)),
                        ("region", gcp_region(r, graph)?),
                    ]),
                )],
                outputs: own_id(),
            }
        }
        "firewall" => gcp_firewall(r)?,
        "address" => {
            let mut body = map(vec![
                ("name", gcp_name(r)),
                ("region", gcp_region(r, graph)?),
            ]);
            gcp_labels(r, &mut body);
            Expansion {
                order: Vec::new(),
                resources: vec![primary(r, "google_compute_address", body)],
                outputs: outputs(&[
                    ("address", format!("${{{id}.address}}")),
                    ("id", format!("${{{id}.id}}")),
                ]),
            }
        }
        "disk" => gcp_disk(r, graph)?,
        _ => gcp_vm(r, graph)?,
    })
}

/// A Google Cloud firewall rule is one direction and one source or
/// destination, so each rule of the common firewall is a rule of its own,
/// applied to the machines that carry the firewall's network tag. The
/// first stands for the firewall.
fn gcp_firewall(r: &Resource) -> Result<Expansion, CompileError> {
    if r.params.contains_key("location") {
        return Err(refuse_param(
            r,
            "location",
            "a Google Cloud firewall rule is global, like its network",
        ));
    }
    if r.params.contains_key("tags") {
        return Err(refuse_param(
            r,
            "tags",
            "a Google Cloud firewall rule carries no labels",
        ));
    }
    let rules = firewall_rules(r);
    if rules.is_empty() {
        return Err(refuse(
            r,
            &rules::MISSING_PARAM,
            "a firewall on Google Cloud is its rules: give at least one. A machine with no \
             firewall already may send anything and receive nothing"
                .to_owned(),
        ));
    }
    let id = r.id.as_str();
    let network = ref_param(r, "network").unwrap_or_default();
    let tag = gcp_target_tag(id);
    let name = name_text(r).replace('_', "-");
    let mut resources = Vec::new();
    let mut any_out = false;
    for (i, rule) in rules.iter().enumerate() {
        let inbound = rule.direction != "out";
        any_out |= !inbound;
        let mut allow = map(vec![(
            "protocol",
            Value::from(match rule.protocol.as_str() {
                "any" => "all",
                p => p,
            }),
        )]);
        if let Some((a, b)) = rule.ports {
            let ports = if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            };
            allow.insert("ports".to_owned(), Value::List(vec![Value::String(ports)]));
        }
        let body = map(vec![
            ("name", Value::String(format!("{name}-{i}"))),
            ("network", reference(&network, "id")),
            (
                "direction",
                Value::from(if inbound { "INGRESS" } else { "EGRESS" }),
            ),
            ("priority", Value::Int(1000 + i as i64)),
            (
                if inbound {
                    "source_ranges"
                } else {
                    "destination_ranges"
                },
                Value::List(vec![Value::from(rule.cidr.as_str())]),
            ),
            ("target_tags", Value::List(vec![tag.clone()])),
            ("allow", Value::List(vec![Value::Map(allow)])),
        ]);
        resources.push(if i == 0 {
            primary(r, "google_compute_firewall", body)
        } else {
            helper(r, &format!("rule_{i}"), "google_compute_firewall", body)
        });
    }
    if any_out {
        // Google Cloud lets every machine send anything; an outbound rule
        // narrows that only with a deny below it, for the firewall's
        // machines alone.
        resources.push(helper(
            r,
            "out_deny",
            "google_compute_firewall",
            map(vec![
                ("name", Value::String(format!("{name}-out-deny"))),
                ("network", reference(&network, "id")),
                ("direction", Value::from("EGRESS")),
                ("priority", Value::Int(65534)),
                (
                    "destination_ranges",
                    Value::List(vec![Value::from("0.0.0.0/0")]),
                ),
                ("target_tags", Value::List(vec![tag])),
                (
                    "deny",
                    Value::List(vec![Value::Map(map(vec![(
                        "protocol",
                        Value::from("all"),
                    )]))]),
                ),
            ]),
        ));
    }
    Ok(Expansion {
        order: Vec::new(),
        resources,
        outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
    })
}

fn gcp_disk(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    let vm = ref_param(r, "vm");
    let zone =
        match (param(r, "location"), &vm) {
            (Some(zone), _) => zone,
            (None, Some(vm)) => reference(vm, "zone"),
            (None, None) => match text_setting(graph, "location")? {
                Some(Value::String(region)) => Value::String(format!("{region}-b")),
                _ => return Err(refuse(
                    r,
                    &rules::MISSING_PARAM,
                    "a disk on Google Cloud is in a zone: give the param vm, location (such as \
                     europe-west1-b), or spec.settings.location for its region"
                        .to_owned(),
                )),
            },
        };
    let mut body = map(vec![
        ("name", gcp_name(r)),
        ("zone", zone),
        ("size", Value::Uint(infra::disk_size(r))),
        ("type", Value::from("pd-balanced")),
    ]);
    gcp_labels(r, &mut body);
    let mut resources = vec![primary(r, "google_compute_disk", body)];
    if let Some(vm) = vm {
        resources.push(helper(
            r,
            "attachment",
            "google_compute_attached_disk",
            map(vec![
                ("disk", reference(id, "id")),
                ("instance", reference(&vm, "id")),
            ]),
        ));
    }
    Ok(Expansion {
        order: Vec::new(),
        resources,
        outputs: outputs(&[("id", format!("${{{id}.id}}"))]),
    })
}

fn gcp_vm(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    // A vm is in a zone: its location, or the region's first zone.
    let zone = match param(r, "location") {
        Some(zone) => zone,
        None => match text_setting(graph, "location")? {
            Some(Value::String(region)) => Value::String(format!("{region}-b")),
            _ => {
                return Err(refuse(
                    r,
                    &rules::MISSING_PARAM,
                    "a vm on Google Cloud is in a zone: give the param location (such as \
                     europe-west1-b), or spec.settings.location for its region"
                        .to_owned(),
                ))
            }
        },
    };
    let size = r.params.get("size").cloned().unwrap_or(Value::Null);
    let machine_type = match asked_size(&size) {
        Some((cpus, memory)) => Value::from(
            closest_size(GCP_SIZES, cpus, memory)
                .ok_or_else(|| too_big(r, cpus, memory, GCP_SIZES))?,
        ),
        None => size,
    };
    let image = r.params.get("image").cloned().unwrap_or(Value::Null);
    let image = match asked_image(&image) {
        Some((family, version)) => {
            let (_, _, path) = GCP_IMAGES
                .iter()
                .find(|(f, v, _)| *f == family && *v == version)
                .expect("the SDK accepts only images every cloud maps");
            Value::from(*path)
        }
        None => image,
    };
    let mut interface = map(vec![(
        "subnetwork",
        reference(&ref_param(r, "subnet").unwrap_or_default(), "id"),
    )]);
    let address = ref_param(r, "address");
    if let Some(address) = &address {
        interface.insert(
            "access_config".to_owned(),
            Value::List(vec![Value::Map(map(vec![(
                "nat_ip",
                reference(address, "address"),
            )]))]),
        );
    }
    let mut body = map(vec![
        ("name", Value::String(name_text(r).replace('_', "-"))),
        ("machine_type", machine_type),
        ("zone", zone),
        (
            "boot_disk",
            Value::List(vec![Value::Map(map(vec![(
                "initialize_params",
                Value::List(vec![Value::Map(map(vec![("image", image)]))]),
            )]))]),
        ),
        (
            "network_interface",
            Value::List(vec![Value::Map(interface)]),
        ),
    ]);
    if let Some(Value::List(firewalls)) = r.params.get("firewalls") {
        body.insert(
            "tags".to_owned(),
            Value::List(
                firewalls
                    .iter()
                    .filter_map(referenced_id)
                    .map(|f| gcp_target_tag(&f))
                    .collect(),
            ),
        );
    }
    if let Some(key) = param(r, "sshKey") {
        let user = match param(r, "user") {
            Some(Value::String(u)) => u,
            _ => "gceuser".to_owned(),
        };
        let entry = match key {
            Value::String(k) => Value::String(format!("{user}:{k}")),
            // A secret is joined to its user by the tool, at plan time, from
            // the sensitive variable it is handed over as.
            Value::Secret(at) => match graph.secrets.iter().find(|n| n.secret == at) {
                Some(named) => {
                    Value::String(format!("{user}:{}", crate::secret_reference(&named.name)))
                }
                None => Value::from(meridian_sdk::SENSITIVE),
            },
            other => other,
        };
        body.insert(
            "metadata".to_owned(),
            Value::Map(map(vec![("ssh-keys", entry)])),
        );
    } else if r.params.contains_key("user") {
        return Err(refuse_param(
            r,
            "user",
            "on Google Cloud the user is the login an sshKey is for; give the key too",
        ));
    }
    gcp_labels(r, &mut body);
    let public = match &address {
        Some(address) => format!("${{{address}.address}}"),
        None => format!("${{{id}.network_interface[0].access_config[0].nat_ip}}"),
    };
    let mut reads = vec![
        ("id", format!("${{{id}.id}}")),
        (
            "privateAddress",
            format!("${{{id}.network_interface[0].network_ip}}"),
        ),
    ];
    if address.is_some() {
        reads.push(("publicAddress", public));
    }
    Ok(Expansion {
        order: Vec::new(),
        resources: vec![primary(r, "google_compute_instance", body)],
        outputs: outputs(&reads),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_image_the_sdk_accepts_is_mapped_on_both_clouds() {
        for (family, versions) in infra::IMAGE_FAMILIES {
            for version in *versions {
                assert!(
                    AWS_IMAGES
                        .iter()
                        .any(|(f, v, _, _)| f == family && v == version),
                    "aws {family} {version}"
                );
                assert!(
                    AZURE_IMAGES
                        .iter()
                        .any(|(f, v, _, _, _)| f == family && v == version),
                    "azure {family} {version}"
                );
            }
        }
    }

    #[test]
    fn the_size_tables_grow() {
        for table in [AWS_SIZES, AZURE_SIZES, GCP_SIZES] {
            for pair in table.windows(2) {
                assert!(pair[0].1 <= pair[1].1 && pair[0].2 <= pair[1].2, "{pair:?}");
            }
        }
    }
}
