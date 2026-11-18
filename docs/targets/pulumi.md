# Target: `pulumi`

<img src="../branding/logos/meridian-pulumi.svg" width="72" alt="Meridian Pulumi plugin logo">

`target: pulumi` compiles a `kind: Infrastructure` document into a
[Pulumi YAML](https://www.pulumi.com/docs/iac/languages-sdks/yaml/) program and runs it
with the `pulumi` command line.

## What it does and does not support

| | |
|---|---|
| Every resource type of every Pulumi provider | Yes. A resource's `type` is the Pulumi type token and its `params` are that type's properties. |
| Provider functions (data sources) | Yes, as `invoke.<function>`. |
| Explicit providers | Yes. `pulumi:providers:<package>` is a resource like any other. |
| Configuration values and provider settings | Yes, as `config`. |
| Resource options (`protect`, `provider`, `ignoreChanges`, ...) | Yes, under the `resourceOptions` param. |
| References between resources, checked at compile time | Yes. `${id.property}`. |
| `dependsOn` | Yes, between resources. Left out where a reference already implies the order. |
| Outputs, handed to later stages | Yes. Secret outputs are marked sensitive and masked. |
| The [common infrastructure types](#common-infrastructure-types) | Yes, on AWS (`aws`) and Azure (`azure-native`). |
| Plan, apply and outputs | Yes: `pulumi preview`, `pulumi up`, `pulumi stack output`. |
| `notifies` | **No.** Refused at compile time. See [Notifications](#notifications). |
| `runtimeWhen` | **No.** Refused at compile time. |
| `adopt` | Yes: the `import` resource option. See [Adopting what exists](#adopting-what-exists) |
| `state: absent` | **No.** Refused: remove the resource from the document instead. |
| Expressions inside `${}` | **No.** Pulumi YAML reads a name and its properties there, nothing computed. Use Pulumi's `fn::` functions. |
| Reading an existing resource (`get`) and component packages | **Not yet.** `resourceOptions.import` adopts an existing resource. |
| `kind: ResourceSet` documents | **No.** Pulumi provisions infrastructure; it does not configure a host. |

The "No" rows are refused at compile, with what to do instead.

## What you need

- `pulumi` on `PATH`, or `MERIDIAN_PULUMI`. No language runtime is needed.
- Provider credentials, the way each provider expects.
- No Pulumi account unless your backend needs one
  ([The stack and its state](#the-stack-and-its-state)).

## A first document

[`examples/pulumi/pets.yaml`](../../examples/pulumi/pets.yaml) uses the random provider; no
cloud account:

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: pets
spec:
  target: pulumi
  resources:
    - id: prefix
      type: config
      params:
        type: string
        default: web

    - id: pet
      type: random:index:RandomPet
      params:
        prefix: ${prefix}
        length: 2

    - id: password
      type: random:index:RandomPassword
      params:
        length: 24
        special: false
      dependsOn:
        - resource: pet

  outputs:
    - name: pet_name
      value: ${pet.id}
    - name: greeting
      value: hello from ${pet.id} on ${pulumi.stack}
    - name: password
      value: ${password.result}
```

```
meridian compile examples/pulumi/pets.yaml --out build   # write Pulumi.yaml; change nothing
meridian plan examples/pulumi/pets.yaml                  # pulumi preview
meridian apply examples/pulumi/pets.yaml                 # pulumi up
```

It compiles to:

```yaml
name: pets
runtime: yaml
description: Compiled by Meridian from the document pets. Do not edit.
config:
  prefix:
    default: web
    type: string
resources:
  pet:
    type: random:index:RandomPet
    properties:
      length: 2
      prefix: ${prefix}
  password:
    type: random:index:RandomPassword
    properties:
      length: 24
      special: false
    options:
      dependsOn:
        - ${pet}
outputs:
  pet_name: ${pet.id}
  greeting: hello from ${pet.id} on ${pulumi.stack}
  password: ${password.result}
```

[`examples/pulumi/web-vm.yaml`](../../examples/pulumi/web-vm.yaml) is an AWS VM using every
type below.

## Resource types

| `type` | Becomes | `params` are |
|---|---|---|
| `<package>:<module>:<Type>`, such as `aws:ec2:Instance` | an entry under `resources`, named by the resource's `id` | the type's properties, plus `resourceOptions` |
| `pulumi:providers:<package>` | the same: an explicit provider is a resource in Pulumi | the provider's settings |
| `invoke.<function>`, such as `invoke.aws:ec2:getAmi` | an entry under `variables` that calls the function with `fn::invoke` | the function's arguments, plus `invokeOptions` |
| `config` | an entry under `config` | the declaration: `type`, `default`, `secret`, `description`, `items`, or `value` |
| `stack` | nothing in the program | the deployment's settings: `name`, `backend`, `secretsProvider` |

Any well-formed type token compiles; Pulumi checks it. Property names are Pulumi's
(`instanceType`).

### Resources and their options

```yaml
- id: web_vm
  type: aws:ec2:Instance
  params:
    ami: ${ubuntu.id}
    instanceType: t3.small
    resourceOptions:
      protect: true
      ignoreChanges: [ami]
```

`params` are properties, except `resourceOptions`, which holds
[resource options](https://www.pulumi.com/docs/iac/concepts/options/).

### Function calls

```yaml
- id: ubuntu
  type: invoke.aws:ec2:getAmi
  params:
    mostRecent: true
    owners: ["099720109477"]
```

Read as `${ubuntu.id}`. Call options go under `invokeOptions`.

### Explicit providers

```yaml
- id: west
  type: pulumi:providers:aws
  params:
    region: us-west-2

- id: bucket
  type: aws:s3:Bucket
  params:
    resourceOptions:
      provider: ${west}
```

### Configuration

A plain name declares a value (with `type` or `default`), read as `${id}`:

```yaml
- id: instance_type
  type: config
  params:
    type: string
    default: t3.small
```

A package-prefixed name is a provider setting, given with `name` and `value`:

```yaml
- id: region
  type: config
  params:
    name: aws:region
    value: us-east-1
```

`name` also renames a plain value.

## Common infrastructure types

`network`, `subnet`, `firewall`, `address` and `vm` are Meridian's own types,
and this target turns each into the `aws` or `azure-native` package's own
([the common infrastructure types](../reference/dsl/common-infrastructure-types.md)
has every param and mapping). They make the same resources the Terraform
target makes on that cloud, with the same sizes, images, refusals and outputs.

The cloud is `spec.settings.cloud`, or the one whose package the document
already uses: a `config` named `aws:region` or `azure-native:location`, or a
resource of either package. On Azure, `spec.settings.resourceGroup` is
required and `spec.settings.location` is optional; a resource without a
location goes to its resource group's.

| Common type | AWS | Azure |
|---|---|---|
| `network` | `aws:ec2:Vpc` | `azure-native:network:VirtualNetwork` |
| `subnet` | `aws:ec2:Subnet`; with `public: true`, an `aws:ec2:InternetGateway` per network, an `aws:ec2:RouteTable`, `aws:ec2:Route` and `aws:ec2:RouteTableAssociation` | `azure-native:network:Subnet` |
| `firewall` | `aws:ec2:SecurityGroup`, an `aws:vpc:SecurityGroupIngressRule` or `...EgressRule` per rule, and an allow-all egress rule when no `out` rule is given | `azure-native:network:NetworkSecurityGroup`, the rules inside it |
| `address` | `aws:ec2:Eip` | `azure-native:network:PublicIPAddress`, Standard and static |
| `vm` | `aws:ec2:Instance`, an `aws:ec2:KeyPair`, an `invoke.aws:ec2:getAmi` lookup for an image by name, an `aws:ec2:EipAssociation` | `azure-native:compute:VirtualMachine` and an `azure-native:network:NetworkInterface` (`<id>_nic`) |

Compare [`examples/common/web-pulumi-aws.yaml`](../../examples/common/web-pulumi-aws.yaml)
and [`web-pulumi-azure.yaml`](../../examples/common/web-pulumi-azure.yaml) with
the Terraform documents beside them.

## References

`${id.property}` passes through unchanged; unknown ids are refused at compile.

| You write | Meaning |
|---|---|
| `${web_vm.publicIp}` | a property of the resource `web_vm` |
| `${web_vm}` | the resource itself, for `provider`, `parent` and `dependsOn` options |
| `${subnets.ids[0]}`, `${vm.tags["Name"]}` | an element of a property |
| `http://${web_ip.publicDns}/` | a reference inside longer text |
| `${pulumi.stack}`, `${pulumi.project}`, `${pulumi.cwd}` | Pulumi's own names, handed over as written |
| `$${HOME}` | the literal text `${HOME}` |

Computation inside `${}` is refused. Use Pulumi YAML's
[built-in functions](https://www.pulumi.com/docs/iac/languages-sdks/yaml/yaml-language-reference/#built-in-functions):

```yaml
params:
  password:
    fn::secret: ${db_password}
  policy:
    fn::toJSON:
      Version: "2012-10-17"
```

## Ordering

`dependsOn` becomes the `dependsOn` option only where no reference implies the order:

```yaml
- id: web_ip
  type: aws:ec2:Eip
  params:
    instance: ${web_vm.id}     # this already orders web_ip after web_vm
  dependsOn:
    - resource: web_vm         # so nothing is written for this

- id: announce
  type: command:local:Command
  params:
    create: echo ready
  dependsOn:
    - resource: web_ip         # nothing above says so: becomes dependsOn: [${web_ip}]
```

A `dependsOn` to or from a function call or config is refused unless a reference implies
it.

## Notifications

`notifies` is refused (`unsupported-action`). Instead, pass the changing value into a
property and name it in `replaceOnChanges`:

```yaml
- id: cert
  type: aws:acm:Certificate
  params:
    domainName: example.com

- id: listener
  type: aws:lb:Listener
  params:
    loadBalancerArn: ${lb.arn}
    certificateArn: ${cert.arn}       # the listener reads the certificate
    resourceOptions:
      replaceOnChanges: [certificateArn]   # and is replaced when it changes
```

## The stack and its state

One stack per document, in a project named after it; `dev` by default:

```yaml
- id: deployment
  type: stack
  params:
    name: prod                              # default: dev
    backend: s3://acme-pulumi-state         # default: see below
    secretsProvider: awskms://alias/pulumi  # default: the backend's own
```

State location, first match wins:

1. `PULUMI_BACKEND_URL`.
2. `backend` on the `stack` resource (`https://api.pulumi.com` needs `PULUMI_ACCESS_TOKEN`).
3. `.meridian/infrastructure/<document>/pulumi/`.

With file state, no secrets provider and no `PULUMI_CONFIG_PASSPHRASE` or
`PULUMI_CONFIG_PASSPHRASE_FILE`, the plugin sets an **empty** passphrase, which protects
nothing. Set one, or a `secretsProvider`, which applies only when the stack is created.

## Editor schemas

`meridian schema sync` runs `pulumi package get-schema <package>` for the packages documents
name, at `resourceOptions.version` if set ([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | the resource types and `invoke.<function>` calls the documents use: the input properties with which are required, secret or deprecated, the output-only properties as set by Pulumi, and `resourceOptions` or `invokeOptions` with every option Pulumi takes |
| Listed by name | every other resource and function of those packages, with the first paragraph of its description |
| Pinned | each package's version |

`random:index:RandomPet`, `random:RandomPet` and `random:index/randomPet:RandomPet` match
the same type.

## Running

| Meridian | The plugin runs, in the work directory |
|---|---|
| `plan` | `pulumi stack select <stack> --create`, then `pulumi preview --json` |
| `apply` | the same two, then `pulumi up --yes --skip-preview` |
| outputs | `pulumi stack output --json`, once masked and once with `--show-secrets` |

Changes are reported by id and type (`create web_vm: aws:ec2/instance:Instance`);
`replace` is separate from update. The stack resource and default providers are not
reported.

A plain `apply` previews, then updates. With a [saved plan](../operations/pull-requests.md)
it uses `preview --save-plan` and `up --plan`, with `PULUMI_EXPERIMENTAL` set. A run with a
secret saves no plan and previews again, checked by the artifact hash.

A failed preview returns Pulumi's output and the command.

### Outputs and secrets

`outputs` become program outputs, read back with types intact. Pulumi secrets
(`RandomPassword.result`, `fn::secret`) are sensitive: masked in logs, passed on in full.

### Values from earlier stages

Handed-over values go to `preview` and `up` as `--config <name>=<value>` (JSON unless
text), set the same-named `config`, are never logged, and are saved in the stack settings
file.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) becomes a secret config
value, set from stdin.

```yaml
spec:
  target: pulumi
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db
      type: aws:rds/instance:Instance
      params:
        engine: postgres
        instanceClass: db.t3.micro
        allocatedStorage: 20
        username: app
        password: ${{ secret.db_password }}
```

The param becomes `${meridian_secret_db_password}`, declared under `config`:

```yaml
config:
  meridian_secret_db_password:
    secret: true
    type: string
```

The input, the same-named output and stack outputs of either stay `[secret]`. Not
followed:

- outputs derived from a secret input, unless listed in
  `resourceOptions.additionalSecretOutputs` or wrapped in `fn::secret`;
- secrets inside map inputs;
- resource IDs, which are never secret.

When a stage runs, the runner pipes each value to
`pulumi config set --secret meridian_secret_<name>`, Pulumi encrypts it into the stack
settings file, and `pulumi config rm` removes it after the stage or a failed preview.

State encryption is the secrets provider's; an empty passphrase
([see](#the-stack-and-its-state)) protects nothing. Set a `secretsProvider` or passphrase.

A secret in `stack` settings or a `config` declaration is refused
(`unsupported-secret-place`).

Tested against a stand-in (`plugins/pulumi/tests/run.rs`) and real `pulumi` in CI
(`plugins/pulumi/tests/real.rs`).

## Adopting what exists

A resource's `adopt` is the provider's own import identifier of a thing that
already exists. The target writes it as the resource's `import` option, so
`pulumi up` takes the thing into the stack instead of creating it:

```yaml
    - id: logs
      type: aws:s3/bucket:Bucket
      adopt: example-logs
      params:
        bucket: example-logs
```

```yaml
  logs:
    type: aws:s3/bucket:Bucket
    properties:
      bucket: example-logs
    options:
      import: example-logs
```

- `meridian plan` reports the resource as `adopt`.
- Pulumi refuses the import when the properties differ from what exists, and
  `pulumi preview` says which. Make the params match, then change them once the
  resource is in the stack.
- Only a resource can be adopted: `adopt` on a `config`, an `invoke.<function>`,
  a `stack` or a `pulumi:providers:<name>` is refused with `invalid-adopt`, and
  so is an `adopt` that names another identifier than `resourceOptions.import`.

### Moving to another tool

The plugin reports what it manages, read from `pulumi stack export`: each
resource's `id`, by resource id, leaving out the stack and providers.
[`meridian retarget`](../operations/retargeting.md) uses it to write
`adopt:` when a document moves to another tool.

## What is refused

Each names the rule, and the resource with file and line where there is one.

| Rule | When | What to do |
|---|---|---|
| `unsupported-resource-type` | The `type` is not a Pulumi type token, `config`, `stack` or `invoke.<function>`. Terraform's `aws_instance` is the usual cause. | Use the Pulumi token: `aws:ec2:Instance`. |
| `invalid-reference` | `${...}` names an id the document does not declare, holds something computed, reads a provider setting, or appears in a `config` declaration. | Fix the id, or use a `fn::` function. |
| `unsupported-action` | The resource has `notifies`. Nothing in a Pulumi YAML program makes one resource react to a change in another. | Pass the changing value into a property of the other resource and name that property in its `replaceOnChanges` option. |
| `unsupported-runtime-condition` | The resource has `runtimeWhen`. A Pulumi YAML program has no conditional. | Decide before compiling, with `when`. |
| `invalid-adopt` | `adopt` on something that is not a resource, or beside a `resourceOptions.import` that names another identifier. | Put `adopt` on the resource, and remove the option. |
| `unsupported-state` | `state: absent`. | Remove the resource from the document; Pulumi deletes what its program no longer declares. |
| `unsupported-dependency` | A `dependsOn` to or from a `config`, a `stack` or an `invoke.<function>` that no reference implies. | Make the dependent refer to what it needs. |
| `invalid-param` | `resourceOptions` or `invokeOptions` is not a map, an unknown setting on a `stack` or `config`, or a `stack` setting that is not plain text. | |
| `missing-param` | A `config` with neither `type` nor `default`. | |
| `invalid-name` | An id, a `config` name, the stack's name or the document's name that Pulumi would not accept, or an id of `pulumi`. | Ids start with a letter or underscore and hold letters, digits, underscores and hyphens. |
| `duplicate-resource` | A `config` renamed to a name another resource already has. | |
| `duplicate-resource`, `duplicate-output` | Two `stack` resources, or two outputs with one name. | |

## Environment

| Variable | Effect |
|---|---|
| `MERIDIAN_PULUMI` | The program to run, when it is not `pulumi` on `PATH`. |
| `MERIDIAN_PULUMI_ARGS` | Arguments added to `pulumi preview` and `pulumi up`, split like a shell command line (quotes hold a word together): `--parallel 4`. The calls that select the stack and read its outputs get none. |
| `PULUMI_BACKEND_URL` | Where state lives; wins over the document. |
| `PULUMI_CONFIG_PASSPHRASE`, `PULUMI_CONFIG_PASSPHRASE_FILE` | The passphrase for a stack's secrets. Set either and the plugin sets none. |
| `PULUMI_ACCESS_TOKEN` and provider credentials | Passed through to `pulumi` untouched. |

Settings can also go in `meridian.yaml` under `spec.settings.pulumi`; the environment wins.
See the [plugin settings reference](../reference/plugin-settings.md#pulumi).

## How it is tested

- `tests/compile.rs`: golden files in `plugins/pulumi/testdata/`, every mapping and refusal.
- `tests/run.rs`: a stand-in `pulumi`.
- `tests/plugin.rs`: the binary over the plugin protocol.
- `tests/real.rs`: real `pulumi` with the random provider, in the `pulumi` workflow or with
  `MERIDIAN_TEST_REAL_PULUMI=1 cargo test -p meridian-target-pulumi --test real`.

Re-record golden files: `UPDATE_GOLDEN=1 cargo test -p meridian-target-pulumi`.
