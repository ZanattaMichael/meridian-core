# The infrastructure as code flavour

Infrastructure as code tools create things through an API, keep track of what
they created, and can report facts about the result. This page is how the
[Meridian language](index.md) is written for them.

| | |
|---|---|
| Category, as `meridian plugins` prints it | `infrastructure-as-code` |
| Kind of document | `Infrastructure`. A `ResourceSet` is refused: these tools create things, and are not pointed at one machine |
| `type` and `params` | the tool's own, passed through; or one of the [common infrastructure types](common-infrastructure-types.md) on `terraform`, `opentofu`, `cloudformation`, `bicep` and `arm` |
| Outputs | yes |

## Targets

| Target | `type` is | Example | Settings resource | Page |
|---|---|---|---|---|
| `terraform` | the Terraform type | `aws_instance` | `provider`, `backend` | [terraform](../../targets/terraform.md) |
| `cloudformation` | the CloudFormation type | `AWS::EC2::Instance` | `stack` (optional) | [cloudformation](../../targets/cloudformation.md) |
| `bicep` | the Azure type and API version | `Microsoft.Network/publicIPAddresses@2023-09-01` | `deployment` (required) | [bicep](../../targets/bicep.md) |
| `opentofu` | the OpenTofu type | `aws_instance` | `provider`, `backend`, `encryption` | [opentofu](../../targets/opentofu.md) |
| `pulumi` | the Pulumi type token | `aws:ec2:Instance` | `stack`, `config` | [pulumi](../../targets/pulumi.md) |
| `arm` | the Azure type, with the API version in the `apiVersion` param | `Microsoft.Network/publicIPAddresses` | `deployment`, `parameter` | [arm](../../targets/arm.md) |
| `packer` | the part of a build and its Packer type | `source.amazon-ebs`, `provisioner.shell` | `variable`, `local`, `plugin` | [packer](../../targets/packer.md) |

## Native passthrough

A Terraform provider has thousands of resource types and Azure has every
service. Meridian does not put a smaller vocabulary in front of them. A
resource's `type` is the tool's own type, and its `params` are that type's
arguments exactly as the tool's documentation gives them, in the tool's own
spelling (`instance_type` for Terraform, `InstanceType` for CloudFormation).

So a document written with the tool's own types is written for one tool and
does not move to another by changing `spec.target`. The
[common types](#common-types-beside-the-native-ones) below are the exception. What it shares with every other Meridian
document is everything around the resource: the document format, ids,
`dependsOn`, references checked at compile time, outputs, and one way to plan
and apply.

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: machine
spec:
  target: terraform
  resources:
    - id: aws
      type: provider
      params:
        region: eu-west-1

    - id: web_sg
      type: aws_security_group
      params:
        name: web

    - id: web
      type: aws_instance
      params:
        ami: ami-0abcdef1234567890
        instance_type: t3.micro
        vpc_security_group_ids: ["${web_sg.id}"]

  outputs:
    - name: address
      value: ${web.public_ip}
```

This document is
[`examples/flavours/infrastructure-as-code.yaml`](../../../examples/flavours/infrastructure-as-code.yaml).
A longer one for each tool:
[`examples/terraform/web-vm.yaml`](../../../examples/terraform/web-vm.yaml),
[`examples/cloudformation/web-vm.yaml`](../../../examples/cloudformation/web-vm.yaml),
[`examples/bicep/web-vm.yaml`](../../../examples/bicep/web-vm.yaml).

## Common types beside the native ones

Five resource types are Meridian's own and mean the same on every cloud:
`network`, `subnet`, `firewall`, `address` and `vm`. On `terraform` and
`opentofu` (for AWS and Azure) and `bicep`, each becomes the cloud's own resources, and the same
document compiles for each. They are used beside the tool's own types, so a
document mixes `vm` with an `aws_s3_bucket` where the common type falls short.

```yaml
    - id: net
      type: network
      params: {cidr: 10.0.0.0/16}
    - id: web
      type: subnet
      params: {network: "${net}", cidr: 10.0.1.0/24}
    - id: vm
      type: vm
      params:
        subnet: ${web}
        size: {cpus: 2, memoryGb: 4}
        image: {family: ubuntu, version: "24.04"}
        sshKey: ssh-ed25519 AAAA...
```

Every param, what each type becomes on each cloud, the size and image tables,
and what each cloud refuses are on
[the common infrastructure types page](common-infrastructure-types.md). The
[conformance matrix for them](../infra-conformance-matrix.md) shows what each
target and cloud does with about forty small documents.

## The settings resource

Most targets in this segment take a resource that is not something to create.
It says where and how the rest is deployed: a Terraform `provider` and
`backend`, a CloudFormation `stack` (name, region, capabilities), a Bicep
`deployment` (resource group, subscription). It is written as a resource so that
a document stays one list, and it has a fixed type name. The target's page lists
its params and says whether it is required.

## `state`

Left out. These tools have one idea of state: what the document declares exists,
and what it no longer declares is removed (or, on a Bicep resource group
or ARM deployment, left alone; on Packer, nothing is ever removed, because a
build only creates images). `state: absent` is refused on every target in the
segment, with the instruction to remove the resource from the document.

## References

This is the flavour where `${...}` does the most.

| You write | Terraform | CloudFormation | Bicep |
|---|---|---|---|
| `${web}` | the resource's address | `Ref` | the symbolic name |
| `${web.attribute}` | an attribute | `Fn::GetAtt` | a property |
| a reference inside longer text | interpolation | `Fn::Sub` | interpolation |
| something that is not a path | Terraform's expression language | pseudo parameters such as `${AWS::Region}` | Bicep's expression language, such as `${resourceGroup().location}` |

The other targets of the segment follow the same idea in their tool's spelling:

| Target | A reference becomes |
|---|---|
| `opentofu` | the same as Terraform |
| `pulumi` | Pulumi YAML's own `${web.property}`, carried over unchanged. Nothing computed is accepted inside `${}`; use Pulumi's `fn::` functions |
| `arm` | `resourceId(...)` for the resource, `reference(...)` for its state, and `format(...)` inside longer text. ARM's own expressions are written in square brackets |
| `packer` | a read of a `variable`, a `local` or a `data.<type>`. A source's results are read only by an output |

The first name of a path must be a resource of the document, or the document
does not compile. What can be read after it is whatever the tool exposes for
that type.

## Order

`dependsOn` becomes the tool's own keyword (`depends_on`, `DependsOn`,
`dependsOn`). These tools also derive order from references, so where a
reference already puts one resource after another, the dependency is not written
a second time. Two targets differ:

| Target | Difference |
|---|---|
| `arm` | Resource Manager infers no order from a resource id, so every reference is also written as a `dependsOn` |
| `packer` | a build has a fixed shape. `dependsOn` orders provisioners and chains post-processors inside it, and a dependency that contradicts the shape is refused |

## `notifies`

These tools have no handlers. What each can be made to do instead:

| Target | Notify |
|---|---|
| `terraform`, `opentofu` | synthetic: the action `replace` only, aimed at a managed resource. It becomes `replace_triggered_by`, so the notified resource is destroyed and created again. The compile warns |
| `cloudformation` | refused. There is no way to react to a change in another resource |
| `bicep`, `arm`, `pulumi` | refused, for the same reason |
| `packer` | refused. A build runs each step once on a machine it then discards |

## `runtimeWhen`

Refused on every target in the segment. There is no node, and no moment on it,
to check a condition.

## `adopt`

The tool's own identifier of a thing that already exists, so the tool takes it
into its state instead of creating it. See
[adopting what exists](resources.md#adopting-what-exists).

| Target | `adopt` is |
|---|---|
| `terraform`, `opentofu` | an `import` block. Only on a managed resource |
| `pulumi` | the resource's `import` option |
| `bicep`, `arm` | accepted with a warning, and changes nothing: Resource Manager updates a resource of the same name in place |
| `cloudformation`, `packer` | refused |

## Outputs

An output names a value the tool reports once the document is applied:

```yaml
outputs:
  - name: address
    value: ${web.public_ip}
```

It becomes the tool's own output (`output`, `Outputs`, `output`), is read back
after the apply, and is kept for the stages that ask for it. Today that is a
ResourceSet's `targetHost`:

```yaml
targetHost:
  from: infrastructure
  ref: machine
  output: address
```

A value the tool marks sensitive is reported as `(sensitive)` and never printed.
Bicep and ARM report every output as text. Pulumi keeps each output's type and
marks a secret as sensitive. A Packer output reads what the build's manifest
records for a source, such as `${image.id}`.

## Plan and apply

`meridian plan` is the tool's own preview: a Terraform plan, a CloudFormation
change set that is then discarded, an Azure `what-if`. Where the tool cannot
analyse a resource, the plan is reported as unknown. Each tool keeps its own
state and uses its own credentials; Meridian stores neither.
