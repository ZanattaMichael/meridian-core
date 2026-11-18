# A two-stage workflow

Most work has two halves: one tool creates a machine, another configures it.
The second needs something the first produced, usually an address. In Meridian
that hand-off is declared, not scripted.

## The documents

[`examples/workflow/vm-then-web.yaml`](../../examples/workflow/vm-then-web.yaml)
holds two documents separated by `---`. Two files passed to the same command
work the same way.

The first is an **Infrastructure** document. Terraform creates an instance, and
the document declares one output:

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

    - id: web
      type: aws_instance
      params:
        ami: ami-0abcdef1234567890
        instance_type: t3.micro
        tags:
          Name: web

  outputs:
    - name: address
      value: ${web.public_ip}
```

The second is a **ResourceSet**. Its host is whatever `address` turns out to be:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: ansible
  targetHost:
    from: infrastructure
    ref: machine
    output: address
  resources:
    - id: nginx_pkg
      type: package
      params:
        name: nginx

    - id: nginx_svc
      type: service
      state: running
      params:
        name: nginx
        enabled: true
      dependsOn:
        - resource: nginx_pkg
```

## What Meridian does with them

Infrastructure documents always run before ResourceSets, whatever the file
order.

```
$ meridian compile examples/workflow/vm-then-web.yaml --out build
machine (Infrastructure, target terraform)
  compiled 1 file into build/machine
site (ResourceSet, target ansible)
  deferred: needs the output "address" of Infrastructure "machine", which has not been applied yet
```

The second stage is **deferred**: its playbook needs a host that does not exist
yet, and Meridian does not compile for a placeholder. `meridian plan` says the
same until the machine exists.

```
$ meridian apply examples/workflow/vm-then-web.yaml
```

applies the first stage, reads `address` from Terraform, then compiles and
applies the second stage for that address. If the first stage fails, the
second does not run. Once the infrastructure exists, `plan` covers both stages.

Outputs and each tool's working files are kept under `.meridian` (change with
`--state-dir` or `MERIDIAN_STATE_DIR`), so later runs can read them. Values a
tool marks sensitive print as `(sensitive)`.

## Which targets can hand something on

A stage can declare outputs only if its tool reports values after an apply. The
`outputs` column of the [capability matrix](../reference/capability-matrix.md)
lists them: Terraform, OpenTofu, CloudFormation, Bicep, ARM templates, Pulumi, Packer, Docker, Kubernetes and Helm. The
configuration management targets configure a machine and report nothing.

Why the two kinds are kept apart:
[the two-stage architecture](../concepts/two-stage-architecture.md).

Next: [worked workflows](workflows.md) has larger workflows with several
machines, several tools per machine, labels, and conditions decided from data.
