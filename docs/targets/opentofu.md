# Target: `opentofu`

<img src="../branding/logos/meridian-opentofu.svg" width="72" alt="Meridian OpenTofu plugin logo">

Compiles an Infrastructure document into an OpenTofu configuration and runs it
with `tofu`.

## What it does and does not support

| | |
|---|---|
| Resource types | OpenTofu's own: any `<provider>_<type>`, plus `data.<type>`, `ephemeral.<type>`, `module`, `variable`, `local`, `provider`, `backend` and `encryption` |
| State and plan encryption | Yes, with `type: encryption`, checked at compile time. See [State and plan encryption](#state-and-plan-encryption) |
| `dependsOn` | Yes, as `depends_on`, unless a reference already implies the order |
| `notifies` | Synthetic, and blunt: the action `replace` only. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused |
| `state: absent` | No. OpenTofu removes what the configuration no longer declares, so remove the resource from the document |
| Plan | Yes: OpenTofu's own plan |
| Outputs | Yes |
| Compiles to | `main.tofu.json`, which only OpenTofu reads |
| Runs | `tofu`, version 1.8 or later; 1.11 or later for `ephemeral.<type>` |
| Plugin binary | `meridian-target-opentofu` |
| Import | Yes: `*.tf`, `*.tofu` and their JSON forms. See [Importing](#importing) |

## A first document

Every resource's `type` is the OpenTofu type, and its `params` are that type's
arguments exactly as the provider documents them. Meridian does not put a smaller
vocabulary in front of OpenTofu's. What it adds is the wiring: `${id.attribute}`
refers to another resource of the same document by its `id`.

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web-vm
spec:
  target: opentofu
  resources:
    - id: aws
      type: provider
      params:
        source: hashicorp/aws
        version: "~> 5.0"
        region: us-east-1

    - id: web_vm
      type: aws_instance
      params:
        ami: ami-0abcdef1234567890
        instance_type: t3.small

    - id: web_ip
      type: aws_eip
      params:
        instance: ${web_vm.id}

  outputs:
    - name: web_vm_ip
      value: ${web_ip.public_ip}
```

```sh
meridian compile web-vm.yaml --out build   # writes build/web-vm/main.tofu.json
meridian plan web-vm.yaml                  # tofu init, tofu plan, and a summary of changes
meridian apply web-vm.yaml                 # applies exactly the plan it showed
```

Two complete documents are in [`examples/opentofu/`](../../examples/opentofu):

- [`web-vm.yaml`](../../examples/opentofu/web-vm.yaml) is a virtual machine on AWS with a
  fixed address, a variable and a local value.
- [`encrypted-state.yaml`](../../examples/opentofu/encrypted-state.yaml) encrypts its state
  and plan. It uses only resources built into OpenTofu, so it runs without a cloud account.

## Resource types

| `type` | Becomes | Referred to as |
|---|---|---|
| `aws_instance`, `terraform_data`, any `<provider>_<type>` | a `resource` block named by the `id` | `${id.attribute}` |
| `data.<type>` | a `data` block | `${id.attribute}` |
| `ephemeral.<type>` | an `ephemeral` block | `${id.attribute}` |
| `module` | a `module` block; `source` is required | `${id.output}` |
| `variable` | a `variable` block | `${id}` |
| `local` | one entry of `locals`, taken from the param `value` | `${id}` |
| `provider` | a `provider` block; `source` and `version` move to `required_providers` | not referable |
| `backend` | the `backend` block; the `id` is the backend type, such as `s3` | not referable |
| `encryption` | the `encryption` block | not referable |

Meta-arguments such as `count`, `for_each`, `provider`, `lifecycle` and `provisioner` are
params like any other, and are written through unchanged.

### Resources and data sources

A managed resource's `type` is the provider's type name and its `params` are that type's
arguments and nested blocks, written as the provider's documentation names them. A data
source is the same with `data.` in front of the type.

```yaml
    - id: ubuntu
      type: data.aws_ami
      params:
        most_recent: true
        owners: ["099720109477"]
        filter:
          - name: name
            values: ["ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*"]

    - id: web_vm
      type: aws_instance
      params:
        ami: ${ubuntu.id}
        instance_type: t3.small
        count: 2
        lifecycle:
          create_before_destroy: true
```

A nested block is a map, and a repeated nested block is a list of maps, as `filter` is
above. OpenTofu decides from the provider's schema whether a param is an argument or a
block, so the document does not have to say.

The only field of a resource besides `id`, `type` and `params` that this target accepts
is `state: present`, which is also the default. `state: absent` is refused: remove the
resource from the document and OpenTofu destroys it.

### Ephemeral resources

`ephemeral.<type>` becomes an `ephemeral` block (OpenTofu 1.11 or later). OpenTofu opens
an ephemeral resource during a plan or an apply and closes it afterwards, and never
writes its values to the state or the plan. Use one for a secret that is only needed
while OpenTofu runs, and pass its values to a provider's write-only arguments:

```yaml
    - id: db_password
      type: ephemeral.aws_secretsmanager_secret_version
      params:
        secret_id: prod/db/password

    - id: db
      type: aws_db_instance
      params:
        identifier: web-db
        password_wo: ${db_password.secret_string}
        password_wo_version: 1
```

An ephemeral resource never appears among a plan's changes.

### Modules

`module` becomes a `module` block. `source` is required; `version`, `providers`, `count`,
`for_each` and `depends_on` are OpenTofu's own, and every other param is an input of the
module. `${id.<output>}` reads one of the module's outputs.

```yaml
    - id: network
      type: module
      params:
        source: terraform-aws-modules/vpc/aws
        version: 5.1.0
        cidr: 10.0.0.0/16

    - id: web_vm
      type: aws_instance
      params:
        subnet_id: ${network.public_subnets[0]}
```

### Backends

`backend` becomes the `backend` block, which says where the state is stored. The `id` is
the backend type and the params are that backend's settings. A document may declare one.
Without one, OpenTofu keeps `terraform.tfstate` in the work directory.

```yaml
    - id: s3
      type: backend
      params:
        bucket: meridian-state
        key: web-vm.tfstate
        region: eu-west-1
```

### Providers

A provider's `id` is its local name, and its params are the provider's configuration.
`source` and `version` are moved to `required_providers`; leave them out for a provider
OpenTofu can find by its name alone. To configure one provider twice, give both the same
`name` and all but one an `alias`:

```yaml
    - id: east
      type: provider
      params: {name: aws, region: us-east-1}
    - id: west
      type: provider
      params: {name: aws, alias: west, region: us-west-2}
    - id: logs
      type: aws_s3_bucket
      params: {bucket: logs, provider: aws.west}
```

### Variables and local values

A `variable` is an input. Its params are OpenTofu's: `type`, `default`, `description`,
`sensitive`, `nullable`, `validation`. A `local` has one param, `value`.

```yaml
    - id: region
      type: variable
      params:
        type: string
        default: eu-west-1
        validation:
          condition: ${startswith(region, "eu-")}
          error_message: Only European regions are allowed.

    - id: name_prefix
      type: local
      params:
        value: web-${region}
```

A variable gets its value from `TF_VAR_<id>` in the environment `meridian` runs in, the
same way it would with `tofu` itself. A value the document
[imports](../reference/dsl/outputs-and-hand-offs.md#imports) from another document of the
workflow arrives the same way, under the import's name, so the import and the variable
share a name. A string is passed as
it is; a number, a boolean, a list or a map is passed as JSON, which OpenTofu parses for a
variable of that type.

## References

`${id.path}` is rewritten to OpenTofu's own address for the resource `id`:

| In the document | In the configuration |
|---|---|
| `${web_vm.id}` where `web_vm` is an `aws_instance` | `${aws_instance.web_vm.id}` |
| `${ubuntu.id}` where `ubuntu` is a `data.aws_ami` | `${data.aws_ami.ubuntu.id}` |
| `${token.value}` where `token` is an `ephemeral.vault_token` | `${ephemeral.vault_token.token.value}` |
| `${network.subnet_ids[0]}` where `network` is a `module` | `${module.network.subnet_ids[0]}` |
| `${region}` where `region` is a `variable` | `${var.region}` |
| `${name_prefix}` where `name_prefix` is a `local` | `${local.name_prefix}` |

The same names are found inside a larger expression, so OpenTofu's functions and operators
can be used on them directly:

```yaml
        ami: ${upper(ubuntu.id)}
        instance_type: '${big ? "t3.large" : "t3.micro"}'
        tags: ${merge(tags, {Role = "database"})}
```

A name is left alone where it does not stand for a value: a function being called, an
attribute after a dot, a key of an object, a name bound by `for`, and the text of a quoted
string. A `${…}` template inside a quoted string is an expression again, so
`${[for zone in ["a", "b"] : "${region}${zone}"]}` works as it reads. Expressions that start with one of OpenTofu's own namespaces (`var`,
`local`, `data`, `module`, `path`, `terraform`, `tofu`, `each`, `count`, `self`,
`ephemeral`, `key_provider`, `method`) are handed over as written, so anything OpenTofu
can express remains available. A resource may not use one of those names as its `id`.

A reference to an `id` the document does not declare is refused at compile time, with the
file and line of the resource that made it, instead of surfacing later as an OpenTofu
error on the machine that runs the plan.

To write a literal `${`, escape it as `$${`.

## Ordering

OpenTofu orders resources from the references between them, so a reference is all the
ordering most documents need. `dependsOn` becomes `depends_on` only when no reference
already implies it:

```yaml
    - id: web_ip
      type: aws_eip
      params:
        instance: ${web_vm.id}
      dependsOn:
        - resource: web_vm     # implied by the reference: nothing is written

    - id: dns_ready
      type: terraform_data
      params:
        input: ready
      dependsOn:
        - resource: web_ip     # written as depends_on
```

Resources, data sources, ephemeral resources and modules can be depended on. Providers,
backends, variables, local values and the encryption block cannot: OpenTofu works those
out before or alongside everything else, and the compile says so.

## Notifications

OpenTofu has no handler to run when something changes. The nearest it has is
`replace_triggered_by`, which destroys and recreates the notified resource. The plugin
declares this as a synthetic notify, accepts only `action: replace`, and warns about every
use, because a replacement can lose data:

```yaml
    - id: config
      type: terraform_data
      params:
        input: v1
      notifies:
        - resource: service
          action: replace
```

```text
opentofu: service: is destroyed and created again whenever "config" changes; OpenTofu has no gentler way to react to a change (replace_triggered_by)
```

A `lifecycle` the document already gives the notified resource, such as
`create_before_destroy`, is kept.

## State and plan encryption

OpenTofu can encrypt its state and plan files. An `encryption` resource compiles to the
`encryption` block, and a document may have one.

```yaml
    - id: state_passphrase
      type: variable
      params:
        type: string
        sensitive: true

    - id: state_encryption
      type: encryption
      params:
        key_provider:
          pbkdf2:
            main:
              passphrase: ${state_passphrase}
        method:
          aes_gcm:
            main:
              keys: ${key_provider.pbkdf2.main}
        state:
          method: ${method.aes_gcm.main}
          enforced: true
        plan:
          method: ${method.aes_gcm.main}
          enforced: true
```

```sh
TF_VAR_state_passphrase='correct-horse-battery-staple' \
  meridian apply examples/opentofu/encrypted-state.yaml
```

The params are OpenTofu's: `key_provider` and `method` are maps of a type to a name to
its settings, and `state`, `plan` and `remote_state_data_sources` each name the method
that protects them. `fallback` and `enforced` are supported, as is the `unencrypted`
method OpenTofu provides for moving an existing state into or out of encryption.

| Param | Shape | Settings |
|---|---|---|
| `key_provider` | `<type>: <name>: {…}` | The key provider's own settings, passed through. For `pbkdf2`, `passphrase`; for `aws_kms`, `kms_key_id`, `region` and `key_spec`. Any other type OpenTofu has, such as `gcp_kms` or `openbao`, is written as given |
| `method` | `<type>: <name>: {…}` | For `aes_gcm`, `keys`, a reference to a key provider. `unencrypted` takes none |
| `state`, `plan` | a map | `method` (required), `enforced` (true or false), `fallback: {method: …}` |
| `remote_state_data_sources` | a map | `default`, and `remote_state_data_source: <name>:`, each with the same settings as `state` |

A `method` is always one fixed reference, `${method.<type>.<name>}`. OpenTofu chooses the
method before it evaluates anything, so an expression cannot choose it; `keys` and a key
provider's own settings may be computed.

The block is checked when the document is compiled, so these mistakes are caught before
anything runs:

| Rule | Refused when |
|---|---|
| `unknown-reference` | `state`, `plan`, a `fallback` or a remote state names a method the block does not declare, or a method takes its `keys` from a key provider the block does not declare |
| `missing-param` | the block protects nothing; a target names no `method`; an `aes_gcm` method has no `keys` |
| `invalid-param` | an argument is not part of an encryption block; a `method` is an expression, or it or `keys` is not written as a reference; `enforced` is not true or false |
| `duplicate-encryption` | the document declares two encryption blocks |

And these are compiled, with a warning:

- **A passphrase written in the document.** It would also be in `main.tofu.json`, beside
  the state it protects. Use a sensitive `variable`, as above.
- **An encrypted state with an unencrypted plan.** A saved plan holds a copy of the
  state, and Meridian saves one in the work directory on every run.
- **A method or key provider that nothing uses.** It is almost always a misspelling of
  the one that was meant.

The passphrase for `pbkdf2` must be at least 16 characters; OpenTofu enforces that when it
runs.

## Outputs

Each entry of `outputs` becomes an `output` block, and its value is reported under the same
name after an apply, which is how a later stage learns the address of the machine this one
creates.

An output that reads a `variable` marked `sensitive: true` is itself written with
`sensitive = true`. OpenTofu refuses to plan an output that could reveal a secret unless
the output says so, and Meridian masks a sensitive output wherever it is shown.

## What running does

| Step | Command |
|---|---|
| Prepare | writes `main.tofu.json` into the work directory, then `tofu init -input=false` |
| Plan | `tofu plan -input=false -out=meridian.tfplan`, then `tofu show -json meridian.tfplan` to read the changes |
| Apply | the plan steps, then `tofu apply -input=false meridian.tfplan`, then `tofu output -json` |
| Outputs | `tofu output -json` |

An apply always applies the plan it has just shown, so nothing changes that was not
reported. The work directory is `<state-dir>/infrastructure/<document>/opentofu`
(`.meridian` by default). OpenTofu keeps `.terraform`, the lock file and, unless the
document declares a `backend`, its state file there, so keep that directory between runs.

Changes are reported as create, update, delete or replace against the resource's `id`. A
resource inside a module, or one of several instances made with `count` or `for_each`,
keeps OpenTofu's full address.

| Environment variable | Effect |
|---|---|
| `MERIDIAN_TOFU` | The program to run, when it is not `tofu` on `PATH` |
| `TF_VAR_<name>` | The value of the variable `<name>` |
| Anything else OpenTofu reads | Passed through, such as `AWS_PROFILE`, `TF_CLI_ARGS` or `TF_ENCRYPTION` |

## What the target does not do

Meridian never degrades silently. Where OpenTofu has no equivalent, the compile fails and
says why.

| Rule | Refused when |
|---|---|
| `unsupported-resource-type` | a `type` is neither a provider's type nor one of the types above, such as `package` |
| `invalid-reference` | `${id.…}` names something the document does not declare, or a provider, backend or encryption block |
| `reserved-name` | an `id` is one of OpenTofu's own namespaces, such as `var` or `data` |
| `invalid-name` | an `id` or output name is not a valid OpenTofu name |
| `unsupported-state` | `state: absent`. OpenTofu destroys what its configuration no longer declares, so remove the resource from the document |
| `unsupported-runtime-condition` | a resource has `runtimeWhen`. OpenTofu computes its whole plan before it changes anything |
| `unsupported-action` | a `notifies` uses an action other than `replace` |
| `unknown-dependency` | `dependsOn` or `notifies` names a resource the document does not declare |
| `unsupported-dependency` | `dependsOn` names, or is written on, a provider, backend, variable, local value or encryption block |
| `duplicate-provider`, `duplicate-backend`, `duplicate-output` | the same thing is declared twice |
| `missing-param` | a `module` has no `source`, or a `local` has no `value` |

The compile needs no provider schema, because the configuration is written in OpenTofu's
JSON syntax, where OpenTofu itself decides whether a param is an argument or a nested
block. The cost is that a misspelled argument is only found by `tofu plan`, which has the
schema. That failure is reported with OpenTofu's own message.

## OpenTofu or Terraform?

The two tools read the same language, and a document that uses none of OpenTofu's own
features compiles to the same blocks for either target. Choose `target: opentofu` when you
run `tofu`, and especially when you want what only OpenTofu has:

| | `opentofu` | `terraform` |
|---|---|---|
| Program | `tofu` | `terraform` |
| File | `main.tofu.json`, which Terraform ignores | `main.tf.json` |
| State and plan encryption | `encryption` | not available |
| `variable`, `local`, `ephemeral.<type>` as resource types | yes | no |
| Document names inside functions and operators | rewritten | handed over as written |
| Sensitive outputs | marked automatically | not marked |

To move a document from Terraform to OpenTofu, change `target: terraform` to
`target: opentofu`. Each target has its own work directory, so copy the existing state
file (`terraform.tfstate`) from `.meridian/infrastructure/<document>/terraform` to
`.meridian/infrastructure/<document>/opentofu` first, or declare a `backend` so both read
the same remote state. Follow OpenTofu's own migration guide for the version you are
moving from.

## Importing

`meridian import --from opentofu DIR` reads a directory's `*.tf`, `*.tofu`, `*.tf.json`
and `*.tofu.json` files and writes one Infrastructure document. The language is
Terraform's, so everything on [the Terraform page](terraform.md#importing) holds here:
the same mapping, the same renaming of names that collide, the same things reported. A
`*.tofu` file replaces the `*.tf` file of the same name, as it does for `tofu`.
[Importing existing configuration](../operations/importing.md) is the guide.

`--from opentofu` is needed for `*.tf` files, which Terraform reads too. A directory that
holds only `*.tofu` files is OpenTofu's without being told.

What OpenTofu has and Terraform does not is imported as well:

| In the configuration | In the document |
|---|---|
| `terraform { encryption { ... } }` | a resource of type `encryption`. `key_provider` and `method` blocks become its params, and `state` and `plan` keep the method they name |
| `ephemeral "random_password" "db"` | type `ephemeral.random_password` |
| `key_provider.pbkdf2.main`, `method.aes_gcm.main` inside the encryption block | kept as written: they name parts of the same block, not resources |

A passphrase written in the configuration is carried into the document as it is, and the
compile-time checks under [State and plan encryption](#state-and-plan-encryption) then
apply to it. Take it from a variable instead.

The round-trip tests are in
[`plugins/opentofu/tests/import.rs`](../../plugins/opentofu/tests/import.rs): the
Terraform page's `web-vm` configuration compiled for OpenTofu, and a configuration with
state encryption.

## Testing

The plugin's tests are in [`plugins/opentofu/tests`](../../plugins/opentofu/tests):

| Tests | What they prove | Needs |
|---|---|---|
| Unit tests in `src/` | expression rewriting, encryption checks, reading plans and outputs | nothing |
| `compile.rs` | every mapping and refusal, against golden files in `testdata/` | nothing |
| `run.rs` | the exact commands, environment and error handling, against a stand-in for `tofu` | nothing |
| `process.rs` | the shipped binary gives the same answers across the plugin protocol | nothing |
| `import.rs` | importing a configuration and compiling it back gives the same configuration, by meaning | nothing |
| `real.rs` | plan, apply, outputs, encryption, notify and validation against a real `tofu` | OpenTofu |

```sh
cargo test -p meridian-target-opentofu
MERIDIAN_TEST_TOFU=tofu cargo test -p meridian-target-opentofu --test real
UPDATE_GOLDEN=1 cargo test -p meridian-target-opentofu    # re-record the golden files
```

The `opentofu` workflow installs OpenTofu and runs the real tests on every pull request
that touches the plugin.
