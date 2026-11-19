# A two-stage workflow

One tool creates a machine and another configures it, using the address the first
reports.

## The documents

[`examples/workflow/vm-then-web.yaml`](../../examples/workflow/vm-then-web.yaml)
holds two documents separated by `---`. Two files work the same way.

An **Infrastructure** document: Terraform creates an instance and declares one output:

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

A **ResourceSet** whose host is that `address`:

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

Infrastructure documents run before ResourceSets.

```
$ meridian compile examples/workflow/vm-then-web.yaml --out build
machine (Infrastructure, target terraform)
  compiled 1 file into build/machine
site (ResourceSet, target ansible)
  deferred: needs the output "address" of Infrastructure "machine", which has not been applied yet
```

The second stage is **deferred** until its host exists. `meridian plan` says the same.

```
$ meridian apply examples/workflow/vm-then-web.yaml
```

applies the first stage, reads `address`, then compiles and applies the second. If the
first fails, the second does not run. Once the machine exists, `plan` covers both.

Outputs and tool working files are kept under `.meridian` (`--state-dir` or
`MERIDIAN_STATE_DIR`). Sensitive values print as `(sensitive)`.

## Which targets can hand something on

Terraform, OpenTofu, CloudFormation, Bicep, ARM, Pulumi, Packer, Docker, Kubernetes and
Helm report outputs (the `outputs` column of the
[capability matrix](../reference/capability-matrix.md)). Configuration management targets
report none. See [the two-stage architecture](../concepts/two-stage-architecture.md).

Next: [worked workflows](workflows.md).
