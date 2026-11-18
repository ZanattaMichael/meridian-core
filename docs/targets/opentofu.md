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
| `adopt` | Yes: an `import` block. See [Adopting what exists](#adopting-what-exists) |
| `state: absent` | No. OpenTofu removes what the configuration no longer declares, so remove the resource from the document |
| Plan | Yes: OpenTofu's own plan |
| Outputs | Yes |
| Compiles to | `main.tofu.json`, which only OpenTofu reads |
| Runs | `tofu`, version 1.8 or later; 1.11 or later for `ephemeral.<type>` |
| Plugin binary | `meridian-target-opentofu` |
| Import | Yes: `*.tf`, `*.tofu` and their JSON forms. See [Importing](#importing) |

## A first document

`type` is the OpenTofu type and `params` its arguments, as the provider documents them.
`${id.attribute}` refers to another resource by `id`.

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

- [`web-vm.yaml`](../../examples/opentofu/web-vm.yaml): an AWS VM with a variable and a local.
- [`encrypted-state.yaml`](../../examples/opentofu/encrypted-state.yaml): encrypted state and
  plan, no cloud account needed.

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

Meta-arguments (`count`, `for_each`, `provider`, `lifecycle`, `provisioner`) are params.

### Resources and data sources

A data source is a resource with `data.` before the type.

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

A nested block is a map; a repeated one is a list of maps. `state` must be `present`; to
destroy, remove the resource.

### Ephemeral resources

`ephemeral.<type>` becomes an `ephemeral` block (OpenTofu 1.11+), never written to state or
plan. Pass its values to write-only arguments:

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

It never appears in a plan's changes.

### Modules

`source` is required; `version`, `providers`, `count`, `for_each` and `depends_on` are
OpenTofu's; other params are inputs. `${id.<output>}` reads an output.

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

The `id` is the backend type; params are its settings. At most one; without it, state is
in the work directory.

```yaml
    - id: s3
      type: backend
      params:
        bucket: meridian-state
        key: web-vm.tfstate
        region: eu-west-1
```

### Providers

The `id` is the local name. `source` and `version` go to `required_providers`. For two
configurations, share `name` and give all but one an `alias`:

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

`variable` params: `type`, `default`, `description`, `sensitive`, `nullable`, `validation`.
A `local` has one param, `value`.

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

Values come from `TF_VAR_<id>`. An [import](../reference/dsl/outputs-and-hand-offs.md#imports)
arrives the same way under its name. Non-strings are passed as JSON.

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

Names inside larger expressions are rewritten too:

```yaml
        ami: ${upper(ubuntu.id)}
        instance_type: '${big ? "t3.large" : "t3.micro"}'
        tags: ${merge(tags, {Role = "database"})}
```

Function names, attributes after a dot, object keys, `for` names and quoted text are left
alone; `${…}` inside quotes is an expression again. OpenTofu namespaces (`var`, `local`,
`data`, `module`, `path`, `terraform`, `tofu`, `each`, `count`, `self`, `ephemeral`,
`key_provider`, `method`) pass through and cannot be ids.

Unknown ids are refused at compile. `$${` is a literal `${`.

## Ordering

`dependsOn` becomes `depends_on` only where no reference implies it:

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

Providers, backends, variables, locals and `encryption` cannot be depended on.

## Notifications

A synthetic notify via `replace_triggered_by`, which recreates the resource. Only
`action: replace`, always with a warning:

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

An existing `lifecycle` is kept.

## State and plan encryption

An `encryption` resource (at most one) compiles to the `encryption` block.

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

`key_provider` and `method` map type to name to settings; `state`, `plan` and
`remote_state_data_sources` name their method. `fallback`, `enforced` and `unencrypted`
are supported.

| Param | Shape | Settings |
|---|---|---|
| `key_provider` | `<type>: <name>: {…}` | The key provider's own settings, passed through. For `pbkdf2`, `passphrase`; for `aws_kms`, `kms_key_id`, `region` and `key_spec`. Any other type OpenTofu has, such as `gcp_kms` or `openbao`, is written as given |
| `method` | `<type>: <name>: {…}` | For `aes_gcm`, `keys`, a reference to a key provider. `unencrypted` takes none |
| `state`, `plan` | a map | `method` (required), `enforced` (true or false), `fallback: {method: …}` |
| `remote_state_data_sources` | a map | `default`, and `remote_state_data_source: <name>:`, each with the same settings as `state` |

A `method` is a fixed `${method.<type>.<name>}`; `keys` and key provider settings may be
computed. Refused at compile:

| Rule | Refused when |
|---|---|
| `invalid-reference` | `state`, `plan`, a `fallback` or a remote state names a method the block does not declare, or a method takes its `keys` from a key provider the block does not declare |
| `missing-param` | the block protects nothing; a target names no `method`; an `aes_gcm` method has no `keys` |
| `invalid-param` | an argument is not part of an encryption block; a `method` is an expression, or it or `keys` is not written as a reference; `enforced` is not true or false |
| `duplicate-resource` | the document declares two encryption blocks |

Warned:

- **A literal passphrase**; use a sensitive `variable`.
- **Encrypted state with an unencrypted plan**; the saved plan holds the state.
- **An unused method or key provider**, usually a typo.

`pbkdf2` passphrases need 16+ characters.

## Outputs

Each output becomes an `output` block, reported after apply. One reading a sensitive
variable is `sensitive = true` and masked.

## Editor schemas

As on [Terraform](terraform.md#editor-schemas), with `tofu init -backend=false` and
`tofu providers schema -json`, `ephemeral.<type>` included. The cache is kept apart as
`opentofu`. See [Tool schemas](../tooling/schema-sync.md).

## What running does

| Step | Command |
|---|---|
| Prepare | writes `main.tofu.json` into the work directory, then `tofu init -input=false` |
| Plan | `tofu plan -input=false -out=meridian.tfplan`, then `tofu show -json meridian.tfplan` to read the changes |
| Apply | the plan steps, then `tofu apply -input=false meridian.tfplan`, then `tofu output -json` |
| Outputs | `tofu output -json` |

An apply applies the plan it just showed. Keep the work directory,
`<state-dir>/infrastructure/<document>/opentofu`: it holds `.terraform`, the lock file and,
without a `backend`, the state.

Changes are reported by `id`; module resources and `count`/`for_each` instances keep their
full address.

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) compiles to
`${var.meridian_secret_<name>}`, declared `sensitive`. The value goes in a private variables
file passed as `-var-file`, removed after planning; the plan file is removed after use. A
[saved plan](../operations/pull-requests.md) keeps no plan file for such a stage, so apply
plans again. [Encrypt the state](#state-and-plan-encryption), which holds the arguments.

| Environment variable | Effect |
|---|---|
| `MERIDIAN_TOFU` | The program to run, when it is not `tofu` on `PATH` |
| `MERIDIAN_TOFU_ARGS` | Arguments added to `init`, `plan` and `apply`, split like a shell command line (quotes hold a word together): `-parallelism=4`, `-lock-timeout=30s`. The calls that read the saved plan and the outputs get none |
| `TF_VAR_<name>` | The value of the variable `<name>` |
| Anything else OpenTofu reads | Passed through, such as `AWS_PROFILE`, `TF_CLI_ARGS` or `TF_ENCRYPTION` |

Settings can also go in `meridian.yaml` under `spec.settings.opentofu`; the environment
wins. See the [plugin settings reference](../reference/plugin-settings.md#opentofu).

## Adopting what exists

A resource's `adopt` is the provider's own import identifier of a thing that
already exists. The target compiles it to an `import` block, so OpenTofu takes the
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
- The identifier is the one `tofu import` takes for that type, as the
  provider's documentation gives it. It may be placed from data:
  `adopt: "${{ data.bucket }}"`.
- Import blocks need OpenTofu 1.6 or later, its first release.

## What the target does not do

Refused at compile:

| Rule | Refused when |
|---|---|
| `unsupported-resource-type` | a `type` is neither a provider's type nor one of the types above, such as `package` |
| `invalid-reference` | `${id.…}` names something the document does not declare, or a provider, backend or encryption block |
| `reserved-name` | an `id` is one of OpenTofu's own namespaces, such as `var` or `data` |
| `invalid-name` | an `id` or output name is not a valid OpenTofu name |
| `unsupported-state` | `state: absent`. OpenTofu destroys what its configuration no longer declares, so remove the resource from the document |
| `unsupported-runtime-condition` | a resource has `runtimeWhen`. OpenTofu computes its whole plan before it changes anything |
| `invalid-adopt` | `adopt` on anything but a managed resource |
| `unsupported-action` | a `notifies` uses an action other than `replace` |
| `unknown-dependency` | `dependsOn` or `notifies` names a resource the document does not declare |
| `unsupported-dependency` | `dependsOn` names, or is written on, a provider, backend, variable, local value or encryption block |
| `duplicate-resource`, `duplicate-output` | the same thing is declared twice |
| `missing-param` | a `module` has no `source`, or a `local` has no `value` |

The output is JSON, so no schema is needed; a misspelled argument is found by `tofu plan`.

## OpenTofu or Terraform?

Without OpenTofu-only features, both targets compile the same blocks. OpenTofu-only:

| | `opentofu` | `terraform` |
|---|---|---|
| Program | `tofu` | `terraform` |
| File | `main.tofu.json`, which Terraform ignores | `main.tf.json` |
| State and plan encryption | `encryption` | not available |
| `variable`, `local`, `ephemeral.<type>` as resource types | yes | no |
| Document names inside functions and operators | rewritten | handed over as written |
| Sensitive outputs | marked automatically | not marked |

To switch, change `target` and copy `terraform.tfstate` from
`.meridian/infrastructure/<document>/terraform` to `.../opentofu`, or use a shared
`backend`. Follow OpenTofu's migration guide.

## Importing

`meridian import --from opentofu DIR` reads `*.tf`, `*.tofu`, `*.tf.json` and `*.tofu.json`
into one document, as [on Terraform](terraform.md#importing)
([guide](../operations/importing.md)). A `*.tofu` file replaces the same-named `*.tf`.
Only-`*.tofu` directories need no `--from`. Also imported:

| In the configuration | In the document |
|---|---|
| `terraform { encryption { ... } }` | a resource of type `encryption`. `key_provider` and `method` blocks become its params, and `state` and `plan` keep the method they name |
| `ephemeral "random_password" "db"` | type `ephemeral.random_password` |
| `key_provider.pbkdf2.main`, `method.aes_gcm.main` inside the encryption block | kept as written: they name parts of the same block, not resources |

A literal passphrase is imported as is and then [warned about](#state-and-plan-encryption).
Round-trip tests:
[`plugins/opentofu/tests/import.rs`](../../plugins/opentofu/tests/import.rs).

## Testing

In [`plugins/opentofu/tests`](../../plugins/opentofu/tests):

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
MERIDIAN_TEST_REAL_TOFU=tofu cargo test -p meridian-target-opentofu --test real
UPDATE_GOLDEN=1 cargo test -p meridian-target-opentofu    # re-record the golden files
```

The `opentofu` workflow runs the real tests.
