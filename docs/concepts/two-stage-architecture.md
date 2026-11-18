# The two-stage architecture

Meridian has two kinds of document, `Infrastructure` and `ResourceSet`, and runs
them as two stages. This page explains why.

<p align="center"><img src="../graphics/meridian-two-stage-workflow.svg" alt="An Infrastructure stage creates the machine and promises an output; a ResourceSet stage names that output as its host" width="100%"></p>

## They answer different questions

An Infrastructure document answers "what should exist?" Its tools (Terraform,
CloudFormation, Bicep, ARM templates, Pulumi, Docker, Kubernetes, Helm) create things through an API, keep
track of what they created, and can tell you facts about the result: an address,
an id, a URL.

A ResourceSet answers "how should this machine be set up?" Its tools (Ansible,
Puppet, OpenVox, Chef, DSC) log in to a machine, or run on it, and converge packages,
files and services. They need the machine to exist first, and they need to know
where it is.

Each plugin declares its category, and a ResourceSet aimed at Terraform, or an
Infrastructure document aimed at Ansible, is refused at compile time. Docker
answers both questions: a container project either stands alone or runs on the
daemon of a named machine, so the `docker` target takes either kind.
[The table](../reference/dsl/index.md#the-document) is in the language
reference.

A single graph over both would treat "create a VM" and "install nginx" as the
same sort of step. They are not: one has nowhere to run until the other
finishes.

## The hand-off is one declared value

An Infrastructure document promises **outputs**:

```yaml
outputs:
  - name: address
    value: ${web.public_ip}
```

A ResourceSet names the output it needs as its host:

```yaml
targetHost:
  from: infrastructure
  ref: machine          # the Infrastructure document's name
  output: address
```

That is the whole interface between stages. `meridian validate` checks that the
document and output exist. The value is only known after the first stage is
applied:

- Until then, the ResourceSet is **deferred**, not compiled against a guess.
- Once the first stage is applied, its tool reports the outputs, and the
  ResourceSet is compiled for the real value and applied. Outputs stay with the
  tool; a later run asks again.
- If the first stage fails, later stages do not run.

A ResourceSet for an existing machine names it directly and needs no first
stage:

```yaml
targetHost:
  from: inline
  host: web01.example.com
```

## Order within a workflow

Every Infrastructure document runs before any ResourceSet, whatever the file
order, unless a document [imports](../reference/dsl/outputs-and-hand-offs.md#imports)
a value from another. A document always runs after the ones it needs, so a
cloud-init ResourceSet runs before the Infrastructure document that creates the
machine with its user-data. A workflow is the documents passed to one
`meridian` command, in one file separated by `---` or in several files.

## What this does not do

By default Meridian does not roll back a first stage when a second fails. A run
stops at the failed stage, and
[`apply --resume`](run-safety.md#picking-up-where-a-run-stopped) continues
from there once fixed. With `--on-failure rollback` it
[puts back](run-safety.md#putting-a-run-back) what the run changed by
re-applying the previous version; it cannot remove what a first apply created.

Each tool has its own state and idea of drift: Terraform notices it when it
plans, Ansible and Chef when they run, Puppet and DSC agents on their own
schedule. [`meridian drift`](run-safety.md#finding-drift) asks every tool and
reports each answer. A tool that cannot preview is reported as unknown, never as
in sync.

[Running safely](run-safety.md) covers what Meridian does across stages: one run
at a time, a record of every run, resuming, putting a run back, and finding
drift.

A worked example: [a two-stage workflow](../quickstart/first-two-stage.md).
