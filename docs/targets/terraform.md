# Target: `terraform`

<img src="../branding/logos/meridian-terraform.svg" width="72" alt="Meridian Terraform plugin logo">

Compiles an Infrastructure document into a Terraform configuration and runs it
with `terraform`. OpenTofu's `tofu` takes the same commands.

## What it does and does not support

| | |
|---|---|
| Resource types | Terraform's own: any `<provider>_<type>`, plus `data.<type>`, `module`, `variable`, `local`, `provider` and `backend` |
| Import | Yes: `*.tf` and `*.tf.json`. See [Importing](#importing) |
| `dependsOn` | Yes, as `depends_on`, unless a reference already implies the order |
| `notifies` | Synthetic, and blunt: the action `replace` only. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused |
| `state: absent` | No. Terraform removes what the configuration no longer declares, so remove the resource from the document |
| Plan | Yes: Terraform's own plan |
| Outputs | Yes |

## What a document looks like

A resource's `type` is the Terraform type and its `params` are that type's
arguments, exactly as the provider documents them.

| `type` | Becomes |
|---|---|
| `aws_instance`, `azurerm_resource_group`, any `<provider>_<type>` | a `resource` block named by the resource's id |
| `data.<type>` | a `data` block |
| `module` | a `module` block |
| `provider` | a `provider` block; `source` and `version` go to `required_providers` |
| `backend` | the `backend` block, whose type is the resource's id |
| `variable` | a `variable` block named by the resource's id. Its params are Terraform's: `type`, `default`, `description`, `sensitive`, `nullable`, `validation` |
| `local` | an entry of `locals` named by the resource's id. Its one param is `value` |

A variable gets its value from `TF_VAR_<id>` in the environment `meridian` runs in, as it
would with `terraform` itself. `${region}` refers to a variable or a local value of that
id and becomes `var.region` or `local.region`.

```yaml
    - id: region
      type: variable
      params:
        type: string
        default: eu-west-1

    - id: name_prefix
      type: local
      params:
        value: web-${region}
```

### Values from other documents

A value the document
[imports](../reference/dsl/outputs-and-hand-offs.md#imports) from another
document arrives as a variable's value, under the import's name, so the import
and the variable share a name. A string is passed as it is; a number, a
boolean, a list or a map is passed as JSON. The value is never written into
`main.tf.json`.

```yaml
spec:
  target: terraform
  imports:
    - name: user_data
      from: artifact
      ref: bootstrap
      file: user-data
      sensitive: true
  resources:
    - id: user_data
      type: variable
      params: {type: string, sensitive: true}
    - id: web
      type: aws_instance
      params:
        ami: ami-0abcdef1234567890
        instance_type: t3.micro
        user_data: ${user_data}
```

[`examples/workflow/bootstrap-then-machine.yaml`](../../examples/workflow/bootstrap-then-machine.yaml)
creates a machine with the user-data a cloud-init document compiles to.

## References

`${web_vm.public_ip}` in a param or an output refers to another resource of the
document by its id, and is rewritten to Terraform's own address for it
(`aws_instance.web_vm.public_ip`, or `data.aws_ami.ubuntu.id` for a data
source). A reference to a resource that is not in the document is a compile
error, with the line.

## Why the output is JSON

The configuration is written in Terraform's JSON syntax (`main.tf.json`), not
HCL. In HCL a nested block and a map-valued argument are written differently,
and only the provider's schema says which a param is. In the JSON syntax
Terraform decides that itself, so a document compiles correctly with no schema.
Terraform reads the file like any other configuration.

## Notifications

Terraform has no handlers. The one reaction it can be given is
`replace_triggered_by`: when the notifier changes, the notified resource is
destroyed and created again. So this target accepts a notify with the action
`replace`, aimed at a managed resource, and nothing else. The compile warns that
this is how the notify works, and a `terraform plan` will show the notified
resource as "must be replaced". That is the notify, not a bug.

```yaml
- id: launch_config
  type: aws_launch_template
  params: {...}
  notifies:
    - resource: web_vm
      action: replace
```

## Example

[`examples/terraform/web-vm.yaml`](../../examples/terraform/web-vm.yaml) declares a
provider, a data source, a security group, an instance and an address, and two
outputs. Its compiled configuration is checked in as
[`web-vm.main_tf_json`](../../plugins/terraform/testdata/web-vm.main_tf_json). The
instance, for example:

```yaml
- id: web_vm
  type: aws_instance
  params:
    ami: ${ubuntu.id}
    instance_type: t3.small
    vpc_security_group_ids: ["${web_sg.id}"]
```

```json
"aws_instance": {
  "web_vm": {
    "ami": "${data.aws_ami.ubuntu.id}",
    "instance_type": "t3.small",
    "vpc_security_group_ids": ["${aws_security_group.web_sg.id}"]
  }
}
```

## Importing

`meridian import --from terraform DIR` reads the `*.tf` and `*.tf.json` files of a
directory, as Terraform does, and writes one Infrastructure document. `*.tfvars` files are
values for one run and not configuration, so they are reported and not read.
[Importing existing configuration](../operations/importing.md) is the guide; this is what the Terraform importer does.

`--from` is needed because `*.tf` is also OpenTofu's. Whichever you name is the tool
Meridian will run for the document.

| In the configuration | In the document |
|---|---|
| `resource "aws_instance" "web"` | a resource of type `aws_instance` with the id `web` |
| `data "aws_ami" "ubuntu"` | type `data.aws_ami` |
| `module "vpc"` | type `module` |
| `variable "region"` | type `variable`; its `type`, `default`, `description`, `sensitive`, `nullable` and `validation` are its params |
| `locals { a = 1, b = 2 }` | one resource of type `local` for each value, with the param `value` |
| `provider "aws"` | type `provider`. `source` and `version` come from `required_providers`. A provider with an `alias` gets the id `<name>_<alias>` |
| `terraform { backend "s3" {} }` | type `backend`, with the id `s3` |
| `output "address"` | an entry of `spec.outputs` |
| `depends_on = [aws_vpc.main]` | `dependsOn`, unless a reference already implies the order |
| `lifecycle { replace_triggered_by = [...] }` | `notifies` with the action `replace`, on the resource that triggers |
| `aws_vpc.main.id`, `var.region`, `local.tags`, `module.vpc.id`, written as a value or inside a string | `${main.id}`, `${region}`, `${tags}`, `${vpc.id}`: a reference to the document's resource of that id |
| The same addresses inside a larger expression, such as `merge(local.tags, {...})` or a conditional | kept as Terraform's own addresses inside `${...}`, which the compiled configuration carries unchanged. A renamed resource is renamed there too |
| `count`, `for_each`, `dynamic` blocks, functions, `each.key`, `count.index` | kept as written. They are Terraform expressions and the compiled configuration carries them unchanged |

A nested block is written as a mapping, and a block that may repeat as a list of
mappings, which is how Terraform's JSON syntax spells them.

What is reported:

| In the configuration | What happens | Level |
|---|---|---|
| Two things with the same name, such as `aws_instance.web` and `aws_eip.web` | A document's ids are unique, so the second is renamed `web_aws_eip` and every reference follows. The note gives the `terraform state mv` command that tells an existing state about the new address; without it the resource is destroyed and created again | changed |
| `moved`, `import`, `removed` and `check` blocks | Left out. The first three change what the state records and are done once with the tool; a check has no counterpart | not translated |
| `terraform { required_version }` | Left out. Pin the binary Meridian runs instead | not translated |
| `terraform { cloud {} }` | Left out. Meridian runs the tool itself | not translated |
| An output's `description` and `sensitive` | Left out: a document's output is a name and a value. Without `sensitive` the value is shown in plans and logs unless what it reads is itself sensitive | not translated |
| A template directive, `%{ if ... }` | The string is carried as written and marked: check it | not translated |
| An override file, `override.tf` or `*_override.tf` | Read as an ordinary file, so a block it overrides appears twice. Merge it by hand | changed |
| A provider required and never configured | A `provider` resource with only its source and version | note |
| `.terraform.lock.hcl` | Not read. The tool writes a new lock file where Meridian runs it | note |

The round-trip test imports [`web-vm/`](../../plugins/terraform/testdata/import/web-vm),
compiles the document, and compares `main.tf.json` by meaning with
[`web-vm.expected.tf.json`](../../plugins/terraform/testdata/import/web-vm.expected.tf.json),
which is the same configuration written by hand in the JSON syntax. The imported document
is [`web-vm.meridian.yaml`](../../plugins/terraform/testdata/import/web-vm.meridian.yaml)
and the report is [`web-vm.notes`](../../plugins/terraform/testdata/import/web-vm.notes).

## Running

| | |
|---|---|
| Plan | `terraform init`, then `terraform plan -out=meridian.tfplan`, then `terraform show -json` to read it |
| Apply | the same plan, then `terraform apply meridian.tfplan`, then `terraform output -json` |
| `MERIDIAN_TERRAFORM` | the program to run, when it is not `terraform` on `PATH`; set it to `tofu` for OpenTofu |

What is applied is exactly the plan that was shown. Values the document
[imports](#values-from-other-documents) reach the configuration
as `TF_VAR_<name>` input variables.
Terraform's state stays where the configuration's `backend` puts it; with no
backend it is a local file in the document's directory under the state
directory, so keep that directory.

An output Terraform marks sensitive is reported as sensitive and never printed.

The runner is tested against the real `terraform` command (1.13) on every pull
request that touches it, and the same tests run again with `tofu`: a plan that
creates nothing, an apply checked on disk, a second plan that is in sync, an
update, a replacement, outputs including a sensitive one, and a configuration
Terraform refuses. They use `terraform_data` and a local state file, so a
provider that talks to a cloud has not been run this way. See
[Real-tool tests](../contributing/real-tool-tests.md).
