# Target: `terraform`

<img src="../branding/logos/meridian-terraform.svg" width="72" alt="Meridian Terraform plugin logo">

Compiles an Infrastructure document into a Terraform configuration and runs it
with `terraform`. OpenTofu's `tofu` takes the same commands.

## What it does and does not support

| | |
|---|---|
| Resource types | Terraform's own: any `<provider>_<type>`, plus `data.<type>`, `module`, `variable`, `local`, `provider` and `backend`. Also the [common infrastructure types](#common-infrastructure-types), on AWS and Azure |
| Import | Yes: `*.tf` and `*.tf.json`. See [Importing](#importing) |
| `dependsOn` | Yes, as `depends_on`, unless a reference already implies the order |
| `notifies` | Synthetic, and blunt: the action `replace` only. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused |
| `adopt` | Yes: an `import` block. See [Adopting what exists](#adopting-what-exists) |
| `state: absent` | No. Terraform removes what the configuration no longer declares, so remove the resource from the document |
| Plan | Yes: Terraform's own plan |
| Outputs | Yes |

## What a document looks like

`type` is the Terraform type; `params` are its arguments as the provider documents them.

| `type` | Becomes |
|---|---|
| `aws_instance`, `azurerm_resource_group`, any `<provider>_<type>` | a `resource` block named by the resource's id |
| `data.<type>` | a `data` block |
| `module` | a `module` block |
| `provider` | a `provider` block; `source` and `version` go to `required_providers` |
| `backend` | the `backend` block, whose type is the resource's id |
| `variable` | a `variable` block named by the resource's id. Its params are Terraform's: `type`, `default`, `description`, `sensitive`, `nullable`, `validation` |
| `local` | an entry of `locals` named by the resource's id. Its one param is `value` |

Variables come from `TF_VAR_<id>`. `${region}` becomes `var.region` or `local.region`.

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

An [import](../reference/dsl/outputs-and-hand-offs.md#imports) sets the same-named
variable (non-strings as JSON), never written into `main.tf.json`.

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

## Common infrastructure types

`network`, `subnet`, `firewall`, `address` and `vm` are Meridian's own types,
and this target turns each into the resources of the AWS or the Azure provider
([the common infrastructure types](../reference/dsl/common-infrastructure-types.md)
has every param and mapping). They are mixed freely with Terraform's own
types. The cloud is the one named in `spec.settings`, or the one whose provider
the document configures:

```yaml
spec:
  target: terraform
  settings:
    cloud: azure            # or aws; optional when only one provider is configured
    location: westeurope    # Azure: where a common resource goes unless it says
    resourceGroup: web-prod # Azure: required
```

| Common type | AWS | Azure |
|---|---|---|
| `network` | `aws_vpc` | `azurerm_virtual_network` |
| `subnet` | `aws_subnet`; when public, an internet gateway, a route table and a route | `azurerm_subnet` |
| `firewall` | `aws_security_group` and one rule resource per rule | `azurerm_network_security_group` and one `azurerm_network_security_rule` per rule |
| `address` | `aws_eip` | `azurerm_public_ip` |
| `vm` | `aws_instance`, an `aws_key_pair`, a `data.aws_ami` lookup, an `aws_eip_association` | `azurerm_linux_virtual_machine`, an `azurerm_network_interface` and its firewall association |

The rules and routes are separate resources rather than the inline
`ingress`, `route` or `security_rule` blocks. Those are "attributes as blocks"
in the providers, which Terraform's JSON syntax would need written out in full.
Examples: [`examples/common/web-aws.yaml`](../../examples/common/web-aws.yaml)
and [`web-azure.yaml`](../../examples/common/web-azure.yaml). The OpenTofu target
takes the common types too, with the same mapping and settings.

## References

`${web_vm.public_ip}` becomes `aws_instance.web_vm.public_ip` (`data.aws_ami.ubuntu.id` for
a data source). Unknown ids are compile errors.

## Why the output is JSON

The output is Terraform's JSON syntax (`main.tf.json`), not HCL, so blocks and map arguments
compile correctly without the provider's schema.

## Notifications

Only `replace` on a managed resource, compiled to `replace_triggered_by`, with a warning.
The plan shows "must be replaced".

```yaml
- id: launch_config
  type: aws_launch_template
  params: {...}
  notifies:
    - resource: web_vm
      action: replace
```

## Example

[`examples/terraform/web-vm.yaml`](../../examples/terraform/web-vm.yaml) compiles to
[`web-vm.main_tf_json`](../../plugins/terraform/testdata/web-vm.main_tf_json). The instance:

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

## Adopting what exists

A resource's `adopt` is the provider's own import identifier of a thing that
already exists. The target compiles it to an `import` block, so Terraform takes the
thing into its state on the next apply instead of creating it:

```yaml
    - id: logs
      type: aws_s3_bucket
      adopt: example-logs
      params:
        bucket: example-logs
```

```json
"import": [
  { "id": "example-logs", "to": "aws_s3_bucket.logs" }
]
```

- `meridian plan` reports the resource as `adopt`, or as `update` with
  `adopted` in its summary when the params differ from what exists. After the
  apply the block does nothing, so it can stay.
- Only a managed resource can be adopted. `adopt` on a provider, a backend, a
  `data.<type>`, a module, a variable or a local is refused with
  `invalid-adopt`.
- The identifier is the one `terraform import` takes for that type, as the
  provider's documentation gives it. It may be placed from data:
  `adopt: "${{ data.bucket }}"`.
- Import blocks need Terraform 1.5 or later.

See [`examples/terraform/adopt.yaml`](../../examples/terraform/adopt.yaml).

### Moving to another tool

The plugin reports what it manages, read from `terraform show -json`: each
managed resource's `id`, by resource id. [`meridian retarget`](../operations/retargeting.md)
uses it to write `adopt:` when a document moves to another tool.

## Importing

`meridian import --from terraform DIR` reads `*.tf` and `*.tf.json` into one document
([guide](../operations/importing.md)). `*.tfvars` are reported, not read. `--from` picks
Terraform or OpenTofu.

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

Nested blocks become mappings; repeatable ones lists of mappings. Reported:

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

Round-trip test: [`web-vm/`](../../plugins/terraform/testdata/import/web-vm),
[`web-vm.expected.tf.json`](../../plugins/terraform/testdata/import/web-vm.expected.tf.json),
[`web-vm.meridian.yaml`](../../plugins/terraform/testdata/import/web-vm.meridian.yaml),
[`web-vm.notes`](../../plugins/terraform/testdata/import/web-vm.notes).

## Running

| | |
|---|---|
| Plan | `terraform init`, then `terraform plan -out=meridian.tfplan`, then `terraform show -json` to read it |
| Apply | the same plan, then `terraform apply meridian.tfplan`, then `terraform output -json` |
| `MERIDIAN_TERRAFORM` | the program to run, when it is not `terraform` on `PATH`; set it to `tofu` for OpenTofu |
| `MERIDIAN_TERRAFORM_ARGS` | arguments added to `init`, `plan` and `apply`, split like a shell command line (quotes hold a word together): `-parallelism=4`, `-lock-timeout=30s`. The calls that read the saved plan and the outputs get none |

Settings can also go in `meridian.yaml` under `spec.settings.terraform`; the environment
wins. See the [plugin settings reference](../reference/plugin-settings.md#terraform).

The apply applies exactly the plan shown. State lives in the `backend`, or a local file
under the state directory; keep it. Sensitive outputs are never printed.

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) compiles to
`${var.meridian_secret_<name>}`, declared `sensitive`. The value goes in a private
variables file passed as `-var-file`, removed after planning; the plan file is removed after
use. A [saved plan](../operations/pull-requests.md) keeps no plan file for such a stage, so
apply plans again. State holds the arguments, so use a `backend` that protects it.

## Editor schemas

`meridian schema sync` writes a configuration requiring the documents' providers (each
`provider`'s `source` and `version`, or `hashicorp/<name>`), runs
`terraform init -backend=false` and reads `terraform providers schema -json`
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | the `<provider>_<type>` and `data.<type>` types the documents use: every attribute with its type, whether it is required, optional or set by the provider, sensitive or deprecated; every nested block; and Terraform's meta-arguments `count`, `for_each`, `provider`, `lifecycle`, `provisioner` and `connection` |
| Listed by name | every other resource and data source of those providers, with its description, and `terraform_data` |
| Pinned | each provider's version, from the `.terraform.lock.hcl` that `init` writes in the work directory |

Optional-and-computed attributes may be written; computed-only ones such as `arn` are read
with `${id.arn}`.

CI runs the runner against `terraform` 1.13 and `tofu`: plan, apply, in-sync, update,
replace, outputs, a refused configuration and a secret. They use `terraform_data` and local
state; cloud providers are untested. See [Real-tool tests](../contributing/real-tool-tests.md).
