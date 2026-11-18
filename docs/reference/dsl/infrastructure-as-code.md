# The infrastructure as code flavour

How the [Meridian language](index.md) is written for tools that create things through an
API.

| | |
|---|---|
| Category, as `meridian plugins` prints it | `infrastructure-as-code` |
| Kind of document | `Infrastructure`. A `ResourceSet` is refused: these tools create things, and are not pointed at one machine |
| `type` and `params` | the tool's own, passed through |
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

`type` is the tool's own type; `params` are its arguments in the tool's spelling
(`instance_type` for Terraform, `InstanceType` for CloudFormation). A document is written
for one tool.

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

[`examples/flavours/infrastructure-as-code.yaml`](../../../examples/flavours/infrastructure-as-code.yaml).
Longer ones:
[`examples/terraform/web-vm.yaml`](../../../examples/terraform/web-vm.yaml),
[`examples/cloudformation/web-vm.yaml`](../../../examples/cloudformation/web-vm.yaml),
[`examples/bicep/web-vm.yaml`](../../../examples/bicep/web-vm.yaml).

## The settings resource

A resource with a fixed type name that says where and how the rest is deployed: a
Terraform `provider` and `backend`, a CloudFormation `stack`, a Bicep `deployment`. See
the target's page.

## `state`

Left out. What the document declares exists. What it no longer declares is
removed, except that a Bicep resource group or ARM deployment leaves it alone
and Packer never removes anything (a build only creates images). `state:
absent` is refused on every target in the segment; remove the resource from
the document instead.

## References

| You write | Terraform | CloudFormation | Bicep |
|---|---|---|---|
| `${web}` | the resource's address | `Ref` | the symbolic name |
| `${web.attribute}` | an attribute | `Fn::GetAtt` | a property |
| a reference inside longer text | interpolation | `Fn::Sub` | interpolation |
| something that is not a path | Terraform's expression language | pseudo parameters such as `${AWS::Region}` | Bicep's expression language, such as `${resourceGroup().location}` |

Other targets, in their tool's spelling:

| Target | A reference becomes |
|---|---|
| `opentofu` | the same as Terraform |
| `pulumi` | Pulumi YAML's own `${web.property}`, carried over unchanged. Nothing computed is accepted inside `${}`; use Pulumi's `fn::` functions |
| `arm` | `resourceId(...)` for the resource, `reference(...)` for its state, and `format(...)` inside longer text. ARM's own expressions are written in square brackets |
| `packer` | a read of a `variable`, a `local` or a `data.<type>`. A source's results are read only by an output |

The first name must be a resource of the document. What follows is whatever the tool
exposes.

## Order

`dependsOn` becomes the tool's keyword (`depends_on`, `DependsOn`, `dependsOn`), except
where a reference already implies the order. Two targets differ:

| Target | Difference |
|---|---|
| `arm` | Resource Manager infers no order from a resource id, so every reference is also written as a `dependsOn` |
| `packer` | a build has a fixed shape. `dependsOn` orders provisioners and chains post-processors inside it, and a dependency that contradicts the shape is refused |

## `notifies`

No handlers. Instead:

| Target | Notify |
|---|---|
| `terraform`, `opentofu` | synthetic: the action `replace` only, aimed at a managed resource. It becomes `replace_triggered_by`, so the notified resource is destroyed and created again. The compile warns |
| `cloudformation` | refused. There is no way to react to a change in another resource |
| `bicep`, `arm`, `pulumi` | refused, for the same reason |
| `packer` | refused. A build runs each step once on a machine it then discards |

## `runtimeWhen`

Refused.

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

A value the tool reports after apply:

```yaml
outputs:
  - name: address
    value: ${web.public_ip}
```

It becomes the tool's own output and is handed to stages that use it, such as a
ResourceSet's `targetHost`:

```yaml
targetHost:
  from: infrastructure
  ref: machine
  output: address
```

Sensitive values show as `(sensitive)`. Bicep and ARM report outputs as text. Pulumi keeps
types. A Packer output reads the build manifest, such as `${image.id}`.

## Plan and apply

`meridian plan` is the tool's preview: a Terraform plan, a discarded CloudFormation change
set, an Azure `what-if`. If the tool cannot analyse a resource, the plan is unknown.
Meridian stores no tool state or credentials.
