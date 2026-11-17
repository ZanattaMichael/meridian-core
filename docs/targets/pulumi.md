# Target: `pulumi`

<img src="../branding/logos/meridian-pulumi.svg" width="72" alt="Meridian Pulumi plugin logo">

`target: pulumi` compiles a `kind: Infrastructure` document into a
[Pulumi YAML](https://www.pulumi.com/docs/iac/languages-sdks/yaml/) program and runs it
with the `pulumi` command line. The plugin's category is **infrastructure as code**.

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
| Plan, apply and outputs | Yes: `pulumi preview`, `pulumi up`, `pulumi stack output`. |
| `notifies` | **No.** Refused at compile time. See [Notifications](#notifications). |
| `runtimeWhen` | **No.** Refused at compile time. |
| `state: absent` | **No.** Refused: remove the resource from the document instead. |
| Expressions inside `${}` | **No.** Pulumi YAML reads a name and its properties there, nothing computed. Use Pulumi's `fn::` functions. |
| Reading an existing resource (`get`) and component packages | **Not yet.** `resourceOptions.import` adopts an existing resource. |
| `kind: ResourceSet` documents | **No.** Pulumi provisions infrastructure; it does not configure a host. |

Nothing is degraded silently. A document that asks for something in the "No" rows does
not compile, and the refusal says what to do instead.

## What you need

- `pulumi` on the `PATH` of the machine that runs Meridian, or its path in
  `MERIDIAN_PULUMI`. Pulumi YAML ships inside the `pulumi` binary, so no language runtime
  is needed.
- Credentials for whichever providers the document uses, supplied the way those providers
  expect (for AWS: the usual environment variables or profile).
- Nothing else. No Pulumi account and no `pulumi login` are needed unless you choose a
  backend that wants them; see [The stack and its state](#the-stack-and-its-state).

## A first document

[`examples/pulumi/pets.yaml`](../../examples/pulumi/pets.yaml) needs no cloud account.
Pulumi's random provider makes up a name and a password:

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

`compile` writes this program, which is what `pulumi` is then given:

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

[`examples/pulumi/web-vm.yaml`](../../examples/pulumi/web-vm.yaml) is a fuller document: a
virtual machine on AWS with a fixed public address, using every resource type below.

## Resource types

| `type` | Becomes | `params` are |
|---|---|---|
| `<package>:<module>:<Type>`, such as `aws:ec2:Instance` | an entry under `resources`, named by the resource's `id` | the type's properties, plus `resourceOptions` |
| `pulumi:providers:<package>` | the same: an explicit provider is a resource in Pulumi | the provider's settings |
| `invoke.<function>`, such as `invoke.aws:ec2:getAmi` | an entry under `variables` that calls the function with `fn::invoke` | the function's arguments, plus `invokeOptions` |
| `config` | an entry under `config` | the declaration: `type`, `default`, `secret`, `description`, `items`, or `value` |
| `stack` | nothing in the program | the deployment's settings: `name`, `backend`, `secretsProvider` |

Meridian does not keep a list of Pulumi's types. Any well-formed type token compiles,
and Pulumi itself says so if the provider has no such type. Property names are Pulumi's
(`instanceType`, `vpcSecurityGroupIds`), not Terraform's (`instance_type`).

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

Everything under `params` is a property except `resourceOptions`, which holds
[Pulumi's resource options](https://www.pulumi.com/docs/iac/concepts/options/) and is
written to the resource's `options`. The key is `resourceOptions` and not `options`
because some resource types have a property of their own called `options`.

### Function calls

```yaml
- id: ubuntu
  type: invoke.aws:ec2:getAmi
  params:
    mostRecent: true
    owners: ["099720109477"]
```

The result is read like a resource: `${ubuntu.id}`. Options of the call (`provider`,
`version`, ...) go under `invokeOptions`.

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

A `config` resource is one of two things, told apart by its name.

A plain name declares a value the program takes. It needs a `type` or a `default`, and
the rest of the document reads it as `${id}`:

```yaml
- id: instance_type
  type: config
  params:
    type: string
    default: t3.small
```

A name with a package in front is that provider's setting, takes only `value`, and is
read by the provider alone. Because such a name cannot be a resource id, it is given
with `name`:

```yaml
- id: region
  type: config
  params:
    name: aws:region
    value: us-east-1
```

`name` also renames a plain value, when the name Pulumi should know differs from the id.

## References

`${id.property}` in a param or an output refers to another resource of the document by
its id. Pulumi YAML spells a reference the same way, so the compiled program carries it
unchanged. What Meridian adds is the check: a reference to an id the document does not
declare is refused when the document is compiled, with the file and line of the
resource, not discovered when Pulumi runs.

| You write | Meaning |
|---|---|
| `${web_vm.publicIp}` | a property of the resource `web_vm` |
| `${web_vm}` | the resource itself, for `provider`, `parent` and `dependsOn` options |
| `${subnets.ids[0]}`, `${vm.tags["Name"]}` | an element of a property |
| `http://${web_ip.publicDns}/` | a reference inside longer text |
| `${pulumi.stack}`, `${pulumi.project}`, `${pulumi.cwd}` | Pulumi's own names, handed over as written |
| `$${HOME}` | the literal text `${HOME}` |

Anything computed is not a reference and is refused inside `${}`. Use Pulumi YAML's
[built-in functions](https://www.pulumi.com/docs/iac/languages-sdks/yaml/yaml-language-reference/#built-in-functions),
which are ordinary keys and pass through as written:

```yaml
params:
  password:
    fn::secret: ${db_password}
  policy:
    fn::toJSON:
      Version: "2012-10-17"
```

## Ordering

Pulumi orders resources by the references between them. `dependsOn` in a document
becomes the `dependsOn` resource option only where no reference already implies the
order, so the compiled program reads the way a Pulumi author would have written it:

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

Only resources can be ordered by hand. A function call runs when its arguments are
known and configuration is read before anything runs, so a `dependsOn` to or from
either is refused unless a reference already implies it.

## Notifications

`notifies` is refused on this target, with the rule `unsupported-action`. Nothing in a
Pulumi YAML program makes one resource react to a change in another: there are no
handlers, and the `replaceOnChanges` option watches only a resource's own properties.
Meridian does not fake a reaction it cannot guarantee, so the plugin claims neither a
native nor a synthetic notify in the
[capability matrix](../reference/capability-matrix.md).

What Pulumi does offer is replacement when a resource's own input changes. Pass the
value that changes into a property of the resource that must react, and name that
property in `replaceOnChanges`:

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

A document deploys to one Pulumi stack in a project named after the document. Without a
`stack` resource the stack is called `dev`:

```yaml
- id: deployment
  type: stack
  params:
    name: prod                              # default: dev
    backend: s3://acme-pulumi-state         # default: see below
    secretsProvider: awskms://alias/pulumi  # default: the backend's own
```

Where state lives is decided in this order:

1. `PULUMI_BACKEND_URL` in the environment of the machine running Meridian. This is
   Pulumi's own rule: the variable wins over a project's `backend`.
2. `backend` on the `stack` resource, which is written to the project file.
   `https://api.pulumi.com` is Pulumi Cloud and needs `PULUMI_ACCESS_TOKEN`.
3. Otherwise, the document's work directory under Meridian's state directory
   (`.meridian/infrastructure/<document>/pulumi/` by default), beside the program. No
   account and no login are needed, which is how the Terraform target behaves too.

When state is kept in a file and no secrets provider is named, Pulumi protects secrets
with a passphrase. If neither `PULUMI_CONFIG_PASSPHRASE` nor
`PULUMI_CONFIG_PASSPHRASE_FILE` is set, the plugin sets an **empty** passphrase so that
Pulumi does not stop to ask for one. An empty passphrase protects nothing: treat the
state directory as you would a Terraform state file, or set a passphrase or a
`secretsProvider`.

The stack is created the first time it is planned or applied. `secretsProvider` is used
only then; Pulumi does not change an existing stack's provider this way.

## Editor schemas

`meridian schema sync` reads each package's schema with `pulumi package
get-schema <package>`, fetching the provider plugin if it is not installed,
so an editor completes and checks a resource's properties
([Tool schemas](../tooling/schema-sync.md)). The packages are the ones the
documents' type tokens and `pulumi:providers:<package>` resources name; a
`resourceOptions.version` chooses the package version asked for.

| | |
|---|---|
| Described in full | the resource types and `invoke.<function>` calls the documents use: the input properties with which are required, secret or deprecated, the output-only properties as set by Pulumi, and `resourceOptions` or `invokeOptions` with every option Pulumi takes |
| Listed by name | every other resource and function of those packages, with the first paragraph of its description |
| Pinned | each package's version |

A token is matched by meaning, so `random:index:RandomPet`,
`random:RandomPet` and `random:index/randomPet:RandomPet` are one type
however the document writes it.

## Running

| Meridian | The plugin runs, in the work directory |
|---|---|
| `plan` | `pulumi stack select <stack> --create`, then `pulumi preview --json` |
| `apply` | the same two, then `pulumi up --yes --skip-preview` |
| outputs | `pulumi stack output --json`, once masked and once with `--show-secrets` |

Each change is reported under the id the document gave the resource, with Pulumi's type
beside it (`create web_vm: aws:ec2/instance:Instance`). A replacement is reported as
`replace`, apart from an update, because it is the one that loses data. Pulumi's own
stack resource and the providers it loads by itself are not reported: neither is
something the document declares.

Pulumi has no saved plan to apply, so `apply` previews and then updates. If something
changes between the two, the update follows the infrastructure, not the preview.

When a preview fails, what Pulumi said is returned with the failure, together with the
command that was run.

### Outputs and secrets

A document's `outputs` become the program's `outputs`. After an apply they are read
back with their types intact: a number stays a number, a list a list.

An output Pulumi treats as a secret, such as a `RandomPassword`'s `result` or anything
wrapped in `fn::secret`, is reported as sensitive. Meridian masks a sensitive value
wherever it prints or logs it, and still hands the real value to the stage that needs
it.

### Values from earlier stages

Values Meridian hands to this stage are passed to `pulumi preview` and `pulumi up` as
`--config <name>=<value>`, so a `config` resource with that name receives them in place
of its default. Text is passed as it is and anything else as JSON. The values never
appear in a log or an error message, since one may be a secret another tool produced.
Pulumi saves them in the stack's settings file in the work directory.

## What is refused

Every refusal carries a rule and, where it is about one resource, that resource with its file and line.

| Rule | When | What to do |
|---|---|---|
| `unsupported-resource-type` | The `type` is not a Pulumi type token, `config`, `stack` or `invoke.<function>`. Terraform's `aws_instance` is the usual cause. | Use the Pulumi token: `aws:ec2:Instance`. |
| `invalid-reference` | `${...}` names an id the document does not declare, holds something computed, reads a provider setting, or appears in a `config` declaration. | Fix the id, or use a `fn::` function. |
| `unsupported-action` | The resource has `notifies`. Nothing in a Pulumi YAML program makes one resource react to a change in another. | Pass the changing value into a property of the other resource and name that property in its `replaceOnChanges` option. |
| `unsupported-runtime-condition` | The resource has `runtimeWhen`. A Pulumi YAML program has no conditional. | Decide before compiling, with `when`. |
| `unsupported-state` | `state: absent`. | Remove the resource from the document; Pulumi deletes what its program no longer declares. |
| `unsupported-dependency` | A `dependsOn` to or from a `config`, a `stack` or an `invoke.<function>` that no reference implies. | Make the dependent refer to what it needs. |
| `invalid-param` | `resourceOptions` or `invokeOptions` is not a map, an unknown setting on a `stack` or `config`, or a `stack` setting that is not plain text. | |
| `missing-param` | A `config` with neither `type` nor `default`. | |
| `invalid-name` | An id, a `config` name, the stack's name or the document's name that Pulumi would not accept, or an id of `pulumi`. | Ids start with a letter or underscore and hold letters, digits, underscores and hyphens. |
| `duplicate-name` | A `config` renamed to a name another resource already has. | |
| `duplicate-stack`, `duplicate-output` | Two `stack` resources, or two outputs with one name. | |

## Environment

| Variable | Effect |
|---|---|
| `MERIDIAN_PULUMI` | The program to run, when it is not `pulumi` on `PATH`. |
| `PULUMI_BACKEND_URL` | Where state lives; wins over the document. |
| `PULUMI_CONFIG_PASSPHRASE`, `PULUMI_CONFIG_PASSPHRASE_FILE` | The passphrase for a stack's secrets. Set either and the plugin sets none. |
| `PULUMI_ACCESS_TOKEN` and provider credentials | Passed through to `pulumi` untouched. |

## How it is tested

- `plugins/pulumi/tests/compile.rs` compiles the examples to golden files in
  `plugins/pulumi/testdata/` and exercises every mapping and every refusal above.
- `plugins/pulumi/tests/run.rs` runs the runner against a stand-in for `pulumi` and
  checks the exact commands, the environment and how failures are reported.
- `plugins/pulumi/tests/plugin.rs` starts the real plugin binary and drives it through
  the plugin protocol, as the `meridian` command does.
- `plugins/pulumi/tests/real.rs` plans, applies, changes and reads outputs with the real
  `pulumi` and its random provider. It runs in the `pulumi` workflow, or locally with
  `MERIDIAN_REAL_PULUMI=1 cargo test -p meridian-target-pulumi --test real`.

To record the golden files again after an intended change:
`UPDATE_GOLDEN=1 cargo test -p meridian-target-pulumi`.
