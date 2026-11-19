//! Stage zero: the common infrastructure types (`network`, `subnet`,
//! `firewall`, `address`, `vm`) become resources of Pulumi's `aws` or
//! `azure-native` package, before the rest of the compile sees the document.
//!
//! The vocabulary and its checks are the SDK's ([`meridian_sdk::infra`]);
//! this module holds only the Pulumi mapping for each cloud. Which cloud a
//! document is aimed at is `spec.settings.cloud`, or, when that is not
//! given, the one cloud whose package the document already uses.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::infra::{
    self, asked_image, asked_size, closest_size, cloud_name, firewall_rules, helper, primary,
    referenced_id, Expansion,
};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::NAME;

/// The keys this target reads in `spec.settings`.
pub const SETTINGS_KEYS: [&str; 3] = ["cloud", "location", "resourceGroup"];

/// The clouds the common types map to on this target, and the package of
/// each.
pub const CLOUDS: &[(&str, &str)] = &[("aws", "aws"), ("azure", "azure-native")];

/// AWS sizes, smallest first. The same table the Terraform and
/// CloudFormation targets use, so a document picks the same machine.
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

/// Azure sizes, smallest first. The same table the Terraform and Bicep
/// targets use.
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

/// The AMI owner and name pattern of each image, as the Terraform target
/// looks them up: the publisher's own account.
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

/// The params of each common type this target takes, on any cloud.
pub fn params(name: &str) -> Vec<&'static str> {
    infra::common_params(name).iter().map(|(p, _)| *p).collect()
}

fn refuse(r: &Resource, rule: &rules::Rule, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

fn setting_error(msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::at("", &Default::default(), &rules::INVALID_SETTING, msg)
    }
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

/// The package a resource of the document already uses: `aws` for
/// `aws:ec2:Instance` or a config value `aws:region`.
fn package_of(r: &Resource) -> Option<&str> {
    let token = match r.r#type.as_str() {
        "config" => match r.params.get("name") {
            Some(Value::String(n)) => n.as_str(),
            _ => return None,
        },
        t => t.strip_prefix("invoke.").unwrap_or(t),
    };
    token
        .strip_prefix("pulumi:providers:")
        .or_else(|| token.split(':').next().filter(|_| token.contains(':')))
}

/// Which cloud the document's common resources map to.
fn cloud(graph: &ResourceGraph, first: &Resource) -> Result<&'static str, CompileError> {
    if let Some(value) = graph.settings.get("cloud") {
        return match CLOUDS.iter().find(|(c, _)| value.as_str() == Some(c)) {
            Some((c, _)) => Ok(c),
            None => Err(setting_error(format!(
                "spec.settings.cloud must be one of {}, not {}",
                CLOUDS
                    .iter()
                    .map(|(c, _)| *c)
                    .collect::<Vec<_>>()
                    .join(", "),
                value.canonical()
            ))),
        };
    }
    let used: BTreeSet<&'static str> = graph
        .resources
        .iter()
        .filter_map(package_of)
        .filter_map(|p| CLOUDS.iter().find(|(_, pkg)| *pkg == p).map(|(c, _)| *c))
        .collect();
    match used.len() {
        1 => Ok(used.into_iter().next().expect("one")),
        n => Err(refuse(
            first,
            &rules::MISSING_RESOURCE,
            format!(
                "a {} is mapped to one cloud's resources, and this document {}. Say which in \
                 spec.settings.cloud (aws, azure)",
                first.r#type,
                if n == 0 {
                    "uses neither the aws nor the azure-native package"
                } else {
                    "uses both the aws and the azure-native package"
                }
            ),
        )),
    }
}

fn map(pairs: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn reference(id: &str, attribute: &str) -> Value {
    Value::String(format!("${{{id}.{attribute}}}"))
}

fn param(r: &Resource, name: &str) -> Value {
    r.params.get(name).cloned().unwrap_or(Value::Null)
}

fn ref_param(r: &Resource, name: &str) -> Option<String> {
    r.params.get(name).and_then(referenced_id)
}

fn outputs(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn expansion(resources: Vec<Resource>, reads: &[(&str, String)]) -> Expansion {
    Expansion {
        resources,
        outputs: outputs(reads),
        order: Vec::new(),
    }
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

fn size(r: &Resource, table: &'static [(&'static str, u32, f64)]) -> Result<Value, CompileError> {
    let size = param(r, "size");
    Ok(match asked_size(&size) {
        Some((cpus, memory)) => Value::from(
            closest_size(table, cpus, memory).ok_or_else(|| too_big(r, cpus, memory, table))?,
        ),
        None => size,
    })
}

#[derive(Default)]
struct State {
    cloud: Option<&'static str>,
    gateways: BTreeSet<String>,
}

/// Replaces the common resources of the tree with the cloud's own.
pub(crate) fn lower(graph: &ResourceGraph) -> Result<ResourceGraph, CompileError> {
    let stamp = |e: CompileError| CompileError {
        target: NAME.to_owned(),
        ..e
    };
    let mut state = State::default();
    let lowered = infra::lower(graph, |r, graph| {
        infra::check_common(r).map_err(stamp)?;
        let cloud = match state.cloud {
            Some(c) => c,
            None => {
                let c = cloud(graph, r)?;
                state.cloud = Some(c);
                c
            }
        };
        match cloud {
            "aws" => aws(r, graph, &mut state),
            _ => azure(r, graph),
        }
    })
    .map_err(stamp)?;
    if state.cloud.is_none() {
        for key in ["location", "resourceGroup"] {
            if graph.settings.contains_key(key) {
                return Err(setting_error(format!(
                    "spec.settings.{key} is read only for the common infrastructure types, and \
                     this document has none"
                )));
            }
        }
    }
    let mut seen = BTreeSet::new();
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

// ---- AWS ----

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

fn aws(r: &Resource, graph: &ResourceGraph, state: &mut State) -> Result<Expansion, CompileError> {
    if r.params.contains_key("location") && r.r#type != "subnet" {
        return Err(refuse_param(
            r,
            "location",
            "on AWS a resource is in the region its provider is configured for (aws:region); \
             only a subnet takes a location, its availability zone",
        ));
    }
    let id = r.id.as_str();
    let own_id = || vec![("id", format!("${{{id}.id}}"))];
    Ok(match r.r#type.as_str() {
        "network" => expansion(
            vec![primary(
                r,
                "aws:ec2:Vpc",
                map(vec![
                    ("cidrBlock", param(r, "cidr")),
                    ("enableDnsHostnames", Value::Bool(true)),
                    ("tags", aws_tags(r)),
                ]),
            )],
            &own_id(),
        ),
        "subnet" => {
            let network = ref_param(r, "network").unwrap_or_default();
            let public = matches!(r.params.get("public"), Some(Value::Bool(true)));
            let mut body = map(vec![
                ("vpcId", reference(&network, "id")),
                ("cidrBlock", param(r, "cidr")),
                ("mapPublicIpOnLaunch", Value::Bool(public)),
                ("tags", aws_tags(r)),
            ]);
            if let Some(zone) = r.params.get("location") {
                body.insert("availabilityZone".to_owned(), zone.clone());
            }
            let mut resources = Vec::new();
            if public {
                // One internet gateway per network; giving it the VPC
                // attaches it, so no ordering beyond the references is needed.
                let gateway = format!("{network}_gateway");
                if state.gateways.insert(network.clone()) {
                    let owner = graph
                        .resources
                        .iter()
                        .find(|n| n.id == network)
                        .unwrap_or(r);
                    let mut gw = helper(
                        owner,
                        "gateway",
                        "aws:ec2:InternetGateway",
                        map(vec![("vpcId", reference(&network, "id"))]),
                    );
                    gw.id = gateway.clone();
                    resources.push(gw);
                }
                resources.push(primary(r, "aws:ec2:Subnet", body));
                resources.push(helper(
                    r,
                    "routes",
                    "aws:ec2:RouteTable",
                    map(vec![
                        ("vpcId", reference(&network, "id")),
                        ("tags", aws_tags(r)),
                    ]),
                ));
                resources.push(helper(
                    r,
                    "routes_internet",
                    "aws:ec2:Route",
                    map(vec![
                        ("routeTableId", reference(&format!("{id}_routes"), "id")),
                        ("destinationCidrBlock", Value::from("0.0.0.0/0")),
                        ("gatewayId", reference(&gateway, "id")),
                    ]),
                ));
                resources.push(helper(
                    r,
                    "routes_link",
                    "aws:ec2:RouteTableAssociation",
                    map(vec![
                        ("subnetId", reference(id, "id")),
                        ("routeTableId", reference(&format!("{id}_routes"), "id")),
                    ]),
                ));
            } else {
                resources.push(primary(r, "aws:ec2:Subnet", body));
            }
            expansion(resources, &own_id())
        }
        "firewall" => {
            let network = ref_param(r, "network").unwrap_or_default();
            let mut resources = vec![primary(
                r,
                "aws:ec2:SecurityGroup",
                map(vec![
                    ("name", cloud_name(r)),
                    ("vpcId", reference(&network, "id")),
                    ("tags", aws_tags(r)),
                ]),
            )];
            let mut any_out = false;
            for (i, rule) in firewall_rules(r).iter().enumerate() {
                let (kind, suffix) = if rule.direction == "out" {
                    any_out = true;
                    ("aws:vpc:SecurityGroupEgressRule", format!("out_{i}"))
                } else {
                    ("aws:vpc:SecurityGroupIngressRule", format!("in_{i}"))
                };
                let mut body = map(vec![("securityGroupId", reference(id, "id"))]);
                let cidr_key = if rule.cidr.contains(':') {
                    "cidrIpv6"
                } else {
                    "cidrIpv4"
                };
                body.insert(cidr_key.to_owned(), Value::from(rule.cidr.as_str()));
                match (rule.protocol.as_str(), rule.ports) {
                    ("any", _) => {
                        body.insert("ipProtocol".to_owned(), Value::from("-1"));
                    }
                    ("icmp", _) => {
                        body.insert("ipProtocol".to_owned(), Value::from("icmp"));
                        body.insert("fromPort".to_owned(), Value::Int(-1));
                        body.insert("toPort".to_owned(), Value::Int(-1));
                    }
                    (protocol, ports) => {
                        let (from, to) = ports.unwrap_or((0, 65535));
                        body.insert("ipProtocol".to_owned(), Value::from(protocol));
                        body.insert("fromPort".to_owned(), Value::Int(from.into()));
                        body.insert("toPort".to_owned(), Value::Int(to.into()));
                    }
                }
                resources.push(helper(r, &suffix, kind, body));
            }
            if !any_out {
                // Pulumi, like Terraform, removes the allow-all rule AWS
                // gives a new security group, so the cloud's default is
                // written back.
                resources.push(helper(
                    r,
                    "out_all",
                    "aws:vpc:SecurityGroupEgressRule",
                    map(vec![
                        ("securityGroupId", reference(id, "id")),
                        ("cidrIpv4", Value::from("0.0.0.0/0")),
                        ("ipProtocol", Value::from("-1")),
                    ]),
                ));
            }
            expansion(resources, &own_id())
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
            expansion(
                vec![primary(
                    r,
                    "aws:ec2:Eip",
                    map(vec![("domain", Value::from("vpc")), ("tags", aws_tags(r))]),
                )],
                &[
                    ("address", format!("${{{id}.publicIp}}")),
                    ("id", format!("${{{id}.id}}")),
                ],
            )
        }
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
            "subnetId",
            reference(&ref_param(r, "subnet").unwrap_or_default(), "id"),
        ),
        ("instanceType", size(r, AWS_SIZES)?),
        ("tags", aws_tags(r)),
    ]);
    let image = param(r, "image");
    match asked_image(&image) {
        Some((family, version)) => {
            let (_, _, owner, pattern) = AWS_IMAGES
                .iter()
                .find(|(f, v, _, _)| *f == family && *v == version)
                .expect("the SDK accepts only images every cloud maps");
            resources.push(helper(
                r,
                "image",
                "invoke.aws:ec2:getAmi",
                map(vec![
                    ("mostRecent", Value::Bool(true)),
                    ("owners", Value::List(vec![Value::from(*owner)])),
                    (
                        "filters",
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
            "vpcSecurityGroupIds".to_owned(),
            Value::List(
                firewalls
                    .iter()
                    .filter_map(referenced_id)
                    .map(|f| reference(&f, "id"))
                    .collect(),
            ),
        );
    }
    if let Some(key) = r.params.get("sshKey") {
        resources.push(helper(
            r,
            "key",
            "aws:ec2:KeyPair",
            map(vec![("keyName", cloud_name(r)), ("publicKey", key.clone())]),
        ));
        body.insert(
            "keyName".to_owned(),
            reference(&format!("{id}_key"), "keyName"),
        );
    }
    resources.push(primary(r, "aws:ec2:Instance", body));
    let public = match ref_param(r, "address") {
        Some(address) => {
            resources.push(helper(
                r,
                "address",
                "aws:ec2:EipAssociation",
                map(vec![
                    ("instanceId", reference(id, "id")),
                    ("allocationId", reference(&address, "id")),
                ]),
            ));
            format!("${{{address}.publicIp}}")
        }
        None => format!("${{{id}.publicIp}}"),
    };
    Ok(expansion(
        resources,
        &[
            ("id", format!("${{{id}.id}}")),
            ("privateAddress", format!("${{{id}.privateIp}}")),
            ("publicAddress", public),
        ],
    ))
}

// ---- Azure ----

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

/// The resource group, and the location unless `located` is false: where
/// every Azure resource goes. A resource with no location of its own and no
/// `spec.settings.location` is placed at its resource group's location, as
/// azure-native does when none is given.
fn azure_place(
    r: &Resource,
    graph: &ResourceGraph,
    located: bool,
) -> Result<BTreeMap<String, Value>, CompileError> {
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
    let mut body = map(vec![("resourceGroupName", group)]);
    if located {
        let location = match r.params.get("location") {
            Some(l) => Some(l.clone()),
            None => text_setting(graph, "location")?,
        };
        if let Some(location) = location {
            body.insert("location".to_owned(), location);
        }
        if let Some(tags) = r.params.get("tags") {
            body.insert("tags".to_owned(), tags.clone());
        }
    }
    Ok(body)
}

fn with(mut body: BTreeMap<String, Value>, more: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    for (k, v) in more {
        body.insert(k.to_owned(), v);
    }
    body
}

fn azure(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
    let id = r.id.as_str();
    let own_id = || vec![("id", format!("${{{id}.id}}"))];
    Ok(match r.r#type.as_str() {
        "network" => expansion(
            vec![primary(
                r,
                "azure-native:network:VirtualNetwork",
                with(
                    azure_place(r, graph, true)?,
                    vec![
                        ("virtualNetworkName", cloud_name(r)),
                        (
                            "addressSpace",
                            Value::Map(map(vec![(
                                "addressPrefixes",
                                Value::List(vec![param(r, "cidr")]),
                            )])),
                        ),
                    ],
                ),
            )],
            &own_id(),
        ),
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
            expansion(
                vec![primary(
                    r,
                    "azure-native:network:Subnet",
                    with(
                        azure_place(r, graph, false)?,
                        vec![
                            ("subnetName", cloud_name(r)),
                            ("virtualNetworkName", reference(&network, "name")),
                            ("addressPrefix", param(r, "cidr")),
                        ],
                    ),
                )],
                &own_id(),
            )
        }
        "firewall" => {
            let rules: Vec<Value> = firewall_rules(r)
                .iter()
                .enumerate()
                .map(|(i, rule)| Value::Map(azure_rule(i, rule)))
                .collect();
            expansion(
                vec![primary(
                    r,
                    "azure-native:network:NetworkSecurityGroup",
                    with(
                        azure_place(r, graph, true)?,
                        vec![
                            ("networkSecurityGroupName", cloud_name(r)),
                            ("securityRules", Value::List(rules)),
                        ],
                    ),
                )],
                &own_id(),
            )
        }
        "address" => expansion(
            vec![primary(
                r,
                "azure-native:network:PublicIPAddress",
                with(
                    azure_place(r, graph, true)?,
                    vec![
                        ("publicIpAddressName", cloud_name(r)),
                        (
                            "sku",
                            Value::Map(map(vec![("name", Value::from("Standard"))])),
                        ),
                        ("publicIPAllocationMethod", Value::from("Static")),
                    ],
                ),
            )],
            &[
                ("address", format!("${{{id}.ipAddress}}")),
                ("id", format!("${{{id}.id}}")),
            ],
        ),
        _ => azure_vm(r, graph)?,
    })
}

/// One security rule. Priorities follow the order the document lists the
/// rules in, from 100 up, as on every Azure target.
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
        ("sourcePortRange", Value::from("*")),
        ("destinationPortRange", Value::String(ports)),
        ("sourceAddressPrefix", Value::from(source)),
        ("destinationAddressPrefix", Value::from(destination)),
    ])
}

fn azure_vm(r: &Resource, graph: &ResourceGraph) -> Result<Expansion, CompileError> {
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
    let name_text = match cloud_name(r) {
        Value::String(n) => n,
        _ => r.id.clone(),
    };

    let mut configuration = map(vec![
        ("name", Value::from("primary")),
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
    let mut nic = with(
        azure_place(r, graph, true)?,
        vec![
            (
                "networkInterfaceName",
                Value::String(format!("{name_text}-nic")),
            ),
            (
                "ipConfigurations",
                Value::List(vec![Value::Map(configuration)]),
            ),
        ],
    );
    if let Some(firewall) = firewalls.first() {
        nic.insert(
            "networkSecurityGroup".to_owned(),
            Value::Map(map(vec![("id", reference(firewall, "id"))])),
        );
    }
    let nic_id = format!("{id}_nic");
    let nic = helper(r, "nic", "azure-native:network:NetworkInterface", nic);

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
    let machine = primary(
        r,
        "azure-native:compute:VirtualMachine",
        with(
            azure_place(r, graph, true)?,
            vec![
                ("vmName", Value::String(name_text.clone())),
                (
                    "hardwareProfile",
                    Value::Map(map(vec![("vmSize", size(r, AZURE_SIZES)?)])),
                ),
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
            ],
        ),
    );
    let mut reads = vec![
        ("id", format!("${{{id}.id}}")),
        (
            "privateAddress",
            format!("${{{nic_id}.ipConfigurations[0].privateIPAddress}}"),
        ),
    ];
    if let Some(address) = &address {
        reads.push(("publicAddress", format!("${{{address}.ipAddress}}")));
    }
    Ok(expansion(vec![nic, machine], &reads))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_image_the_sdk_accepts_is_mapped_on_both_clouds() {
        for (family, versions) in infra::IMAGE_FAMILIES {
            for version in *versions {
                assert!(AWS_IMAGES
                    .iter()
                    .any(|(f, v, _, _)| f == family && v == version));
                assert!(AZURE_IMAGES
                    .iter()
                    .any(|(f, v, _, _, _)| f == family && v == version));
            }
        }
    }
}
