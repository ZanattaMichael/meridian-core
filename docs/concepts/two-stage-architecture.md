# The two-stage architecture

`Infrastructure` documents create things. `ResourceSet` documents configure a machine
that exists. Meridian runs them as separate stages.

<p align="center"><img src="../graphics/meridian-two-stage-workflow.svg" alt="An Infrastructure stage creates the machine and promises an output; a ResourceSet stage names that output as its host" width="100%"></p>

## The two kinds

- **Infrastructure** (Terraform, CloudFormation, Bicep, ARM, Pulumi, Docker, Kubernetes,
  Helm): creates things through an API and reports outputs such as an address or id.
- **ResourceSet** (Ansible, Puppet, OpenVox, Chef, DSC): converges packages, files and
  services on a machine that must already exist.

A document of the wrong kind for its target is refused. `docker` takes either
([the table](../reference/dsl/index.md#the-document)).

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

`meridian validate` checks that the document and output exist.

- Until the first stage is applied, the ResourceSet is **deferred**.
- After it is applied, the ResourceSet is compiled for the real value and applied.
  Outputs stay with the tool; a later run asks again.
- If the first stage fails, later stages do not run.

A ResourceSet for an existing machine names it directly and needs no first
stage:

```yaml
targetHost:
  from: inline
  host: web01.example.com
```

## Order within a workflow

A workflow is the documents passed to one `meridian` command, in one file separated by
`---` or in several files. Infrastructure documents run before ResourceSets, unless an
[import](../reference/dsl/outputs-and-hand-offs.md#imports) needs another order: a
cloud-init ResourceSet runs before the Infrastructure document that uses its user-data.

## Failure and drift

- A failed stage stops the run. [`apply --resume`](run-safety.md#picking-up-where-a-run-stopped)
  continues from it.
- `--on-failure rollback` [re-applies the previous version](run-safety.md#putting-a-run-back).
  It cannot remove what a first apply created.
- [`meridian drift`](run-safety.md#finding-drift) asks every tool. A tool that cannot
  preview is reported as unknown.

See [running safely](run-safety.md) and the worked
[two-stage workflow](../quickstart/first-two-stage.md).
