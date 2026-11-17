# Outputs and hand-offs: `outputs`, `targetHost` and `imports`

Resources inside a document are tied together by
[`dependsOn`](dependson.md), [`notifies`](notifies.md) and
[references](references.md). Documents are tied together by exactly one thing:
a value one document has and another asks for. There are two ways to ask.
[`targetHost`](#targethost) asks for the machine a ResourceSet configures.
[`imports`](#imports) asks for any value, for the document's tool.

```mermaid
flowchart LR
    subgraph I["machine: Infrastructure"]
        web["web<br/>aws_instance"] --> out["output: address<br/>${web.public_ip}"]
    end
    subgraph R["site: ResourceSet"]
        th["targetHost<br/>from: infrastructure<br/>ref: machine<br/>output: address"] --> res["nginx_pkg, nginx_svc"]
    end
    out ==>|"known only after machine is applied"| th
```

## `outputs`

Only in an Infrastructure document: `spec.outputs`, a list.

```yaml
outputs:
  - name: address
    value: ${web.public_ip}
  - name: url
    value: http://${web.public_ip}:8080
```

| Field | Required | Value |
|---|---|---|
| `name` | yes | the output's name, unique among the document's outputs. This is what another document asks for |
| `value` | give one | text holding one or more [references](references.md). The core does not insist on it, but an output without a value has nothing to report. It may also hold an [interpolation](interpolation.md), which Meridian places before compiling: `https://${web.public_dns}/${{ data.project }}` |
| `when` | no | a [condition](conditions.md#where-a-when-can-be-written) on the output. False, and the output is not declared. Give it the same condition as the resource the output reads |

```
out.yaml:13:7: spec.outputs[1].name: duplicate output name "o" (first declared at out.yaml:12:7)
```

**What an output is.** A promise that, once this document has been applied, its
tool can report this value. It is not known before: an address does not exist
until the machine does.

**What it becomes.** The tool's own output where it has one (a Terraform
`output`, a CloudFormation `Outputs` entry, a Bicep `output`). Where the tool
has no such thing, the plugin reads the value back itself after the apply (a
`docker inspect` path, a `kubectl get` path).

**What can be read.** Whatever the tool can report about that resource after it
exists. This is often more than a param may read, because a param is fixed
before the apply and an output is read after it:

| Target | An output can read |
|---|---|
| `terraform`, `opentofu` | any attribute of a resource or data source |
| `cloudformation` | `Ref` and `GetAtt` attributes |
| `bicep` | any property; always reported as text |
| `arm` | a resource's id, name and properties; always reported as text |
| `pulumi` | any property of a resource, with its type kept; a secret is reported as sensitive |
| `packer` | what the build's manifest records for a source: `id`, `artifact_id`, `builder_type`, `build_time`, `files` |
| `docker` | a published port (`${app.port.8080}`), or a path into the container's inspection |
| `kubernetes`, `helm` | any path into the live object, such as `${svc.status.loadBalancer.ingress[0].ip}`; `helm` also anything a template can read |

**Strictness.**

- An output the tool has no value for after an apply is an error, not an empty
  value. A ResourceSet is never pointed at an empty host.
- Some values take time to exist (a load balancer's address). The Kubernetes
  target waits for them, for a limited time.
- A value the tool marks sensitive is shown as `(sensitive)` wherever Meridian
  prints, and anything read from a Kubernetes Secret is treated as sensitive.

**Where outputs are kept.** With the tool that owns them. After an apply they
are shown and handed to the stages that asked. On a later run Meridian asks the
tool again what its last apply produced, which is how a `plan` sees
infrastructure that already exists. Meridian's own record under the state
directory says only which document was applied and whether it worked.

**Which targets have outputs.** The `outputs` column of the
[capability matrix](../capability-matrix.md). Today: every infrastructure as
code, containerization and container orchestration target. A ResourceSet has no
`outputs` field, so configuration management targets produce none.

## `targetHost`

Only in a ResourceSet: `spec.targetHost`, the machine the document configures.
A ResourceSet cannot be compiled without one.

### A machine that already exists

```yaml
targetHost:
  from: inline
  host: web01.example.com
```

| Field | Required | Value |
|---|---|---|
| `from` | yes | `inline` |
| `host` | yes | a host name or address, as you would give it to the tool |

The document needs no other document. It compiles straight away.

### A machine another document creates

```yaml
targetHost:
  from: infrastructure
  ref: machine
  output: address
```

| Field | Required | Value |
|---|---|---|
| `from` | yes | `infrastructure` |
| `ref` | yes | the `metadata.name` of an Infrastructure document given to the same command |
| `output` | yes | the `name` of an output that document declares |

### Rules

| Rule | Checked by | Message |
|---|---|---|
| `from` is `inline` or `infrastructure` | `validate` | `unknown source "x" (expected "infrastructure" or "inline")` |
| `host` is given with `inline` | `validate` | `missing required field for from: inline` |
| `ref` and `output` are given with `infrastructure` | `validate` | `missing required field for from: infrastructure` |
| the document named exists | `validate` | `no Infrastructure document named "machine" is loaded` |
| it declares that output | `validate` | `Infrastructure "machine" declares no output "address"` |
| a ResourceSet has a `targetHost` at all | `compile` | `document "web" declares no targetHost` |

Both documents must be given to the same command. The link is by name, within
one workflow, and nowhere else.

### What the host is used for

The plugin writes the host where its tool expects it, and the tool reaches the
machine its own way, with its own credentials:

| Target | The host becomes |
|---|---|
| `ansible` | the play's `hosts`, and the entry in `inventory.ini` |
| `puppet`, `openvox` | the node statement in `site.pp`; another machine is reached with Bolt |
| `salt` | the entry in the top file; another machine is reached with `salt-ssh` |
| `chef` | the machine `chef-run` is pointed at |
| `cloud-init` | the machine's name in `meta-data`, and where an apply can wait for the first boot |
| `dsc` | the machine the runner reaches over WinRM or SSH, as its `runner` resource says |

## `imports`

In either kind of document: `spec.imports`, values handed to the document's
tool from other documents of the workflow. This is how the machine a document
creates is given the user-data another document compiles to, and how one
Infrastructure document uses what another created.

```mermaid
flowchart LR
    subgraph B["bootstrap: ResourceSet, cloud-init"]
        ud["compiles to the file<br/>user-data"]
    end
    subgraph N["network: Infrastructure"]
        vpc["output: vpc_id"]
    end
    subgraph M["machine: Infrastructure, terraform"]
        imp["imports<br/>user_data, vpc_id"] --> var["variable user_data<br/>variable vpc_id"] --> web["web<br/>aws_instance"]
    end
    ud ==>|"from: artifact<br/>known when bootstrap compiles"| imp
    vpc ==>|"from: output<br/>known after network is applied"| imp
```

```yaml
kind: Infrastructure
metadata: {name: machine}
spec:
  target: terraform
  imports:
    - name: user_data
      from: artifact
      ref: bootstrap
      file: user-data
      sensitive: true
    - name: vpc_id
      from: output
      ref: network
      output: vpc_id
  resources:
    - id: user_data
      type: variable
      params: {type: string, sensitive: true}
    - id: vpc_id
      type: variable
      params: {type: string}
    - id: web
      type: aws_instance
      params:
        ami: ami-0abcdef1234567890
        instance_type: t3.micro
        user_data: ${user_data}
```

| Field | Required | Value |
|---|---|---|
| `name` | yes | the name the value arrives under. It becomes the name of the tool's variable, so it starts with a letter or an underscore and holds only letters, digits and underscores. Unique among the document's imports |
| `from` | yes | `artifact` or `output` |
| `ref` | yes | the `metadata.name` of the other document |
| `file` | with `artifact` | the path of a file the other document compiles to, exactly as `meridian compile` lists it |
| `output` | with `output` | the `name` of an output the other document declares |
| `sensitive` | no | `true` or `false`. `true` masks the value wherever Meridian prints |

### The two sources

| | `from: artifact` | `from: output` |
|---|---|---|
| The value | the whole text of one file the other document compiles to | the value the other document's tool reported for one output |
| The other document | either kind | an Infrastructure document, because only they declare outputs |
| Known | as soon as the other document compiles. No tool has to run | once the other document has been applied, in this run or an earlier one |
| Before it is known | the importing document waits only if the other document itself cannot compile yet | the importing document is **deferred**, as a ResourceSet waiting on a host is |
| `plan` before anything is applied | planned, with the value | deferred |

A file is imported as it compiles, not as it was last applied. If the other
document has changed since its last apply, the import already carries the
change, and the run applies the other document first.

### How the value arrives

The value goes to the document's tool, under the import's name. Meridian never
splices it into the document or the compiled files, so a secret imported into
a Terraform document is not written into `main.tf.json`: it arrives where
Terraform's own variables arrive.

| Target | The import `user_data` arrives as | The document declares |
|---|---|---|
| `terraform` | the environment variable `TF_VAR_user_data` | a resource of type `variable` with the id `user_data`, read as `${user_data}` |
| `opentofu` | `TF_VAR_user_data` | a `variable`, read as `${user_data}` |
| `packer` | `PKR_VAR_user_data` | a `variable` |
| `pulumi` | the stack configuration value `user_data` (`--config`), always passed as a secret argument | a `config` value |
| `arm` | the template parameter `user_data` (`--parameters`) | a `parameter`; one Meridian is given no value for is left to `az` |
| every other target | refused | |

A string is passed as it is. A number, a boolean, a list or a map is passed as
JSON.

A target that has no way to hand its tool a value refuses a document that
imports, when it is compiled:

```
the document imports "user_data", and target "ansible" has no way to hand a value from another document to its tool; remove spec.imports, or use a target that takes inputs
```

The [capability matrix](../capability-matrix.md) has an `inputs` column, and
`meridian plugins` lists `inputs` among what a plugin can do.

A ResourceSet whose host comes from an Infrastructure document is also handed
every output of that document, under the output's name. An import of the same
name takes its place.

### Order

A document runs after every document it imports from, and after the one its
`targetHost` comes from. Nothing else changes the order, so a workflow without
imports runs exactly as before.

```mermaid
flowchart LR
    W["as written:<br/>machine, network, bootstrap"] --> O["as run:<br/>network, bootstrap, machine"]
```

- A document the run leaves out, with a [selector](index.md#labels-and-selectors),
  is waited on by nothing. A file imported from it is compiled for the
  purpose and nothing of it is applied; an output imported from it is read
  from what its last apply left behind.
- Documents that wait on each other cannot be ordered. The usual way to write
  one is a machine created with a bootstrap document's user-data, where the
  bootstrap document's `targetHost` is that machine's address. Give the
  bootstrap document an `inline` host: cloud-init runs on the machine itself
  and does not need to reach it.

### Secrets

- `sensitive: true` masks the value in everything Meridian prints, including
  what the tool prints, in the form the tool usually prints it.
- An output its tool marks sensitive stays sensitive when it is imported,
  without `sensitive: true`.
- A value is never recorded. The [run's record](../state.md) names the steps
  and never holds an import's value.
- Mark the tool's own variable sensitive too (`sensitive: true` on a
  Terraform `variable`), so that the tool keeps it out of its plan.

### Rules

| Rule | Checked by | Message |
|---|---|---|
| `name` is given | `validate` | `missing required field` |
| `name` can be a variable's name | `validate` | `"user-data" cannot name an import: it becomes the name of the tool's variable, ...` |
| names are unique in the document | `validate` | `duplicate import name "a" (first declared at ...)` |
| `from` is `artifact` or `output` | `validate` | `unknown source "x" (expected "artifact" or "output")` |
| `file` is given with `artifact`, `output` with `output` | `validate` | `missing required field for from: artifact` |
| the other one is not | `validate` | `does not apply to from: artifact, which takes \`file\`` |
| the document named exists | `validate` | `no document named "bootstrap" is loaded` |
| for an output, it is an Infrastructure document | `validate` | `"bootstrap" is a ResourceSet, and only an Infrastructure document declares outputs` |
| it declares that output | `validate` | `Infrastructure "network" declares no output "vpc_id"` |
| for an artifact, the name means one document | `validate` | `both an Infrastructure and a ResourceSet document are named "network", ...; rename one of them` |
| a document does not import from itself | `validate` | `a document cannot import from itself` |
| no documents wait on each other | `validate` | `these documents wait on each other, so none of them can be applied first: ResourceSet "bootstrap" needs Infrastructure "machine" needs ResourceSet "bootstrap"` |
| the target takes imports | `compile` | `the document imports "user_data", and target "ansible" has no way ...` |
| the file is one the other document compiles to | `plan`, `apply` | `import "user_data" is the file "userdata" of ResourceSet "bootstrap", which compiles to no file of that name (it compiles to: meta-data, user-data)` |
| the output was produced | `apply` | `Infrastructure "network" did not produce the output "vpc_id" that import "vpc_id" needs (it produced: ...)` |

A whole workflow, compiled by the tests on every change:
[`examples/workflow/bootstrap-then-machine.yaml`](../../../examples/workflow/bootstrap-then-machine.yaml).

## The life of a hand-off

```mermaid
sequenceDiagram
    participant U as you
    participant M as meridian
    participant T as terraform
    participant A as ansible
    U->>M: apply machine.yaml site.yaml
    M->>M: validate: machine declares "address", site asks for it
    M->>T: compile and apply "machine"
    T-->>M: outputs: address = 203.0.113.10
    M->>M: site's host is now known
    M->>A: compile "site" for 203.0.113.10, then apply
    A-->>M: done
    M-->>U: both stages applied
```

| Moment | State of the ResourceSet |
|---|---|
| `validate` | checked: the document and output it names exist |
| `compile`, or `plan` before the first apply | **deferred**. Reported, not failed, and not compiled against a guess |
| `apply` | compiled for the real value once the Infrastructure stage has succeeded, then applied |
| `plan` on a later run | planned like any other stage, because the tool can report the output |
| the Infrastructure stage fails | **not run** |

```
$ meridian compile examples/workflow/vm-then-web.yaml --out build
machine (Infrastructure, target terraform)
  compiled 1 file into build/machine
site (ResourceSet, target ansible)
  deferred: needs the output "address" of Infrastructure "machine", which has not been applied yet
```

## What cannot be expressed yet

- An import reaches a tool's variable, on the targets that have variables. It
  cannot be written straight into a param with `${...}`, and a
  configuration-management target such as Ansible cannot import at all yet.
  Reading another document's output inside any value is part of the
  [language expansion](../../plans/language-expansion.md).
- A ResourceSet compiles to files, and those can be imported; it has no
  `outputs` of its own to import.
- A ResourceSet configures one host. A fleet is several documents, or the
  tool's own inventory outside Meridian.

The reasons for the two-stage shape are in
[the two-stage architecture](../../concepts/two-stage-architecture.md), and a
full walk through in [a two-stage workflow](../../quickstart/first-two-stage.md).
