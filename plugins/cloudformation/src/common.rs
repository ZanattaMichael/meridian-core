//! Stage zero: the common infrastructure types (`network`, `subnet`,
//! `firewall`, `address`, `vm`) become CloudFormation resource types, before
//! the rest of the compile sees the document.
//!
//! The vocabulary and its checks are the SDK's ([`meridian_sdk::infra`]);
//! this module holds only the AWS mapping. Every resource is created in the
//! stack's region; only a subnet takes a location, its availability zone.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::infra::{
    self, asked_image, asked_size, closest_size, cloud_name, firewall_rules, helper, primary,
    referenced_id, Expansion,
};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{refuse, NAME};

/// AWS sizes, smallest first: (instance type, processors, memory in GB). The
/// same table the Terraform target uses on AWS, so a document picks the same
/// machine with either tool.
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

/// The public Systems Manager parameter that holds the newest AMI of each
/// image a vm asks for, published by the image's own vendor. CloudFormation
/// reads it when the stack is deployed.
pub const AWS_IMAGES: &[(&str, &str, &str)] = &[
    (
        "debian",
        "11",
        "/aws/service/debian/release/11/latest/amd64",
    ),
    (
        "debian",
        "12",
        "/aws/service/debian/release/12/latest/amd64",
    ),
    (
        "ubuntu",
        "22.04",
        "/aws/service/canonical/ubuntu/server/22.04/stable/current/amd64/hvm/ebs-gp2/ami-id",
    ),
    (
        "ubuntu",
        "24.04",
        "/aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id",
    ),
];

/// The params of each common type this target takes.
pub fn params(name: &str) -> Vec<&'static str> {
    infra::common_params(name).iter().map(|(p, _)| *p).collect()
}

fn map(pairs: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// `${id}`, which becomes a `Ref`.
fn r#ref(id: &str) -> Value {
    Value::String(format!("${{{id}}}"))
}

/// `${id.attribute}`, which becomes `Fn::GetAtt`.
fn attribute(id: &str, attribute: &str) -> Value {
    Value::String(format!("${{{id}.{attribute}}}"))
}

fn param(r: &Resource, name: &str) -> Value {
    r.params.get(name).cloned().unwrap_or(Value::Null)
}

fn ref_param(r: &Resource, name: &str) -> Option<String> {
    r.params.get(name).and_then(referenced_id)
}

/// Tags as CloudFormation lists them, with the resource's name added under
/// `Name`, the way the AWS console shows a resource.
fn tags(r: &Resource) -> Value {
    let mut tags = match r.params.get("tags") {
        Some(Value::Map(m)) => m.clone(),
        _ => BTreeMap::new(),
    };
    tags.entry("Name".to_owned())
        .or_insert_with(|| cloud_name(r));
    Value::List(
        tags.into_iter()
            .map(|(k, v)| Value::Map(map(vec![("Key", Value::String(k)), ("Value", v)])))
            .collect(),
    )
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

/// What the mapping remembers between resources: the internet gateway a
/// network's public subnets share.
#[derive(Default)]
struct State {
    gateways: BTreeSet<String>,
}

/// Replaces the common resources of the tree with CloudFormation's own.
pub(crate) fn lower(graph: &ResourceGraph) -> Result<ResourceGraph, CompileError> {
    let stamp = |e: CompileError| CompileError {
        target: NAME.to_owned(),
        ..e
    };
    let mut state = State::default();
    let lowered = infra::lower(graph, |r, graph| {
        infra::check_common(r).map_err(stamp)?;
        expand(r, graph, &mut state)
    })
    .map_err(stamp)?;
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

fn expand(
    r: &Resource,
    graph: &ResourceGraph,
    state: &mut State,
) -> Result<Expansion, CompileError> {
    if r.params.contains_key("location") && r.r#type != "subnet" {
        return Err(refuse_param(
            r,
            "location",
            "a CloudFormation stack is deployed to one region, set on its stack resource; only a \
             subnet takes a location, its availability zone",
        ));
    }
    let id = r.id.as_str();
    Ok(match r.r#type.as_str() {
        "network" => Expansion {
            resources: vec![primary(
                r,
                "AWS::EC2::VPC",
                map(vec![
                    ("CidrBlock", param(r, "cidr")),
                    ("EnableDnsHostnames", Value::Bool(true)),
                    ("EnableDnsSupport", Value::Bool(true)),
                    ("Tags", tags(r)),
                ]),
            )],
            outputs: outputs(&[("id", format!("${{{id}}}"))]),
            order: Vec::new(),
        },
        "subnet" => subnet(r, graph, state),
        "firewall" => {
            let network = ref_param(r, "network").unwrap_or_default();
            let mut ingress = Vec::new();
            let mut egress = Vec::new();
            for rule in firewall_rules(r) {
                let mut body = BTreeMap::new();
                let cidr_key = if rule.cidr.contains(':') {
                    "CidrIpv6"
                } else {
                    "CidrIp"
                };
                body.insert(cidr_key.to_owned(), Value::from(rule.cidr.as_str()));
                match (rule.protocol.as_str(), rule.ports) {
                    ("any", _) => {
                        body.insert("IpProtocol".to_owned(), Value::from("-1"));
                    }
                    ("icmp", _) => {
                        body.insert("IpProtocol".to_owned(), Value::from("icmp"));
                        body.insert("FromPort".to_owned(), Value::Int(-1));
                        body.insert("ToPort".to_owned(), Value::Int(-1));
                    }
                    (protocol, ports) => {
                        let (from, to) = ports.unwrap_or((0, 65535));
                        body.insert("IpProtocol".to_owned(), Value::from(protocol));
                        body.insert("FromPort".to_owned(), Value::Int(from.into()));
                        body.insert("ToPort".to_owned(), Value::Int(to.into()));
                    }
                }
                if rule.direction == "out" {
                    egress.push(Value::Map(body));
                } else {
                    ingress.push(Value::Map(body));
                }
            }
            let mut body = map(vec![
                ("GroupName", cloud_name(r)),
                ("GroupDescription", cloud_name(r)),
                ("VpcId", r#ref(&network)),
                ("Tags", tags(r)),
            ]);
            if !ingress.is_empty() {
                body.insert("SecurityGroupIngress".to_owned(), Value::List(ingress));
            }
            // With no outbound rule written, AWS keeps the allow-all rule it
            // gives a new security group; one written replaces it.
            if !egress.is_empty() {
                body.insert("SecurityGroupEgress".to_owned(), Value::List(egress));
            }
            Expansion {
                resources: vec![primary(r, "AWS::EC2::SecurityGroup", body)],
                outputs: outputs(&[("id", format!("${{{id}.GroupId}}"))]),
                order: Vec::new(),
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
                resources: vec![primary(
                    r,
                    "AWS::EC2::EIP",
                    map(vec![("Domain", Value::from("vpc")), ("Tags", tags(r))]),
                )],
                outputs: outputs(&[
                    ("address", format!("${{{id}.PublicIp}}")),
                    ("id", format!("${{{id}.AllocationId}}")),
                ]),
                order: Vec::new(),
            }
        }
        _ => vm(r)?,
    })
}

fn subnet(r: &Resource, graph: &ResourceGraph, state: &mut State) -> Expansion {
    let id = r.id.as_str();
    let network = ref_param(r, "network").unwrap_or_default();
    let public = matches!(r.params.get("public"), Some(Value::Bool(true)));
    let mut body = map(vec![
        ("VpcId", r#ref(&network)),
        ("CidrBlock", param(r, "cidr")),
        ("MapPublicIpOnLaunch", Value::Bool(public)),
        ("Tags", tags(r)),
    ]);
    if let Some(zone) = r.params.get("location") {
        body.insert("AvailabilityZone".to_owned(), zone.clone());
    }
    let mut resources = Vec::new();
    let mut order = Vec::new();
    if public {
        // One internet gateway per network, attached to it, which every
        // public subnet of it routes through.
        let gateway = format!("{network}_gateway");
        let link = format!("{network}_gateway_link");
        if state.gateways.insert(network.clone()) {
            let owner = graph
                .resources
                .iter()
                .find(|n| n.id == network)
                .unwrap_or(r);
            let mut gw = helper(
                owner,
                "gateway",
                "AWS::EC2::InternetGateway",
                BTreeMap::new(),
            );
            gw.id = gateway.clone();
            gw.params.insert("Tags".to_owned(), tags(owner));
            let mut attachment = helper(
                owner,
                "gateway_link",
                "AWS::EC2::VPCGatewayAttachment",
                map(vec![
                    ("VpcId", r#ref(&network)),
                    ("InternetGatewayId", r#ref(&gateway)),
                ]),
            );
            attachment.id = link.clone();
            resources.push(gw);
            resources.push(attachment);
        }
        resources.push(primary(r, "AWS::EC2::Subnet", body));
        let table = format!("{id}_routes");
        let route = format!("{id}_routes_internet");
        resources.push(helper(
            r,
            "routes",
            "AWS::EC2::RouteTable",
            map(vec![("VpcId", r#ref(&network)), ("Tags", tags(r))]),
        ));
        resources.push(helper(
            r,
            "routes_internet",
            "AWS::EC2::Route",
            map(vec![
                ("RouteTableId", r#ref(&table)),
                ("DestinationCidrBlock", Value::from("0.0.0.0/0")),
                ("GatewayId", r#ref(&gateway)),
            ]),
        ));
        // A route through a gateway is refused until the gateway is
        // attached, which no reference says.
        order.push((link, route));
        resources.push(helper(
            r,
            "routes_link",
            "AWS::EC2::SubnetRouteTableAssociation",
            map(vec![
                ("SubnetId", r#ref(id)),
                ("RouteTableId", r#ref(&table)),
            ]),
        ));
    } else {
        resources.push(primary(r, "AWS::EC2::Subnet", body));
    }
    Expansion {
        resources,
        outputs: outputs(&[("id", format!("${{{id}}}"))]),
        order,
    }
}

fn vm(r: &Resource) -> Result<Expansion, CompileError> {
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
            "SubnetId",
            r#ref(&ref_param(r, "subnet").unwrap_or_default()),
        ),
        ("Tags", tags(r)),
    ]);
    let size = param(r, "size");
    let instance_type = match asked_size(&size) {
        Some((cpus, memory)) => {
            Value::from(closest_size(AWS_SIZES, cpus, memory).ok_or_else(|| {
                let (largest, c, m) = AWS_SIZES.last().expect("not empty");
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
    body.insert("InstanceType".to_owned(), instance_type);
    let image = param(r, "image");
    let image = match asked_image(&image) {
        Some((family, version)) => {
            let (_, _, path) = AWS_IMAGES
                .iter()
                .find(|(f, v, _)| *f == family && *v == version)
                .expect("the SDK accepts only images every cloud maps");
            Value::String(format!("{{{{resolve:ssm:{path}}}}}"))
        }
        None => image,
    };
    body.insert("ImageId".to_owned(), image);
    if let Some(Value::List(firewalls)) = r.params.get("firewalls") {
        body.insert(
            "SecurityGroupIds".to_owned(),
            Value::List(
                firewalls
                    .iter()
                    .filter_map(referenced_id)
                    .map(|f| attribute(&f, "GroupId"))
                    .collect(),
            ),
        );
    }
    if let Some(key) = r.params.get("sshKey") {
        resources.push(helper(
            r,
            "key",
            "AWS::EC2::KeyPair",
            map(vec![
                ("KeyName", cloud_name(r)),
                ("PublicKeyMaterial", key.clone()),
            ]),
        ));
        body.insert("KeyName".to_owned(), r#ref(&format!("{id}_key")));
    }
    resources.push(primary(r, "AWS::EC2::Instance", body));
    let public = match ref_param(r, "address") {
        Some(address) => {
            resources.push(helper(
                r,
                "address",
                "AWS::EC2::EIPAssociation",
                map(vec![
                    ("InstanceId", r#ref(id)),
                    ("AllocationId", attribute(&address, "AllocationId")),
                ]),
            ));
            format!("${{{address}.PublicIp}}")
        }
        None => format!("${{{id}.PublicIp}}"),
    };
    Ok(Expansion {
        resources,
        outputs: outputs(&[
            ("id", format!("${{{id}}}")),
            ("privateAddress", format!("${{{id}.PrivateIp}}")),
            ("publicAddress", public),
        ]),
        order: Vec::new(),
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
                    AWS_IMAGES
                        .iter()
                        .any(|(f, v, _)| f == family && v == version),
                    "{family} {version}"
                );
            }
        }
    }
}
