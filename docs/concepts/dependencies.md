# Dependencies and notifications

A resource can relate to another in two ways. They look alike but mean
different things, so they are separate fields.

## `dependsOn`: order

```yaml
- id: nginx_conf
  type: file
  params: {path: /etc/nginx/nginx.conf, content: "..."}
  dependsOn:
    - resource: nginx_pkg
```

"Do not touch this until that is done." It is only about order.

Meridian builds a graph from every `dependsOn`, refuses cycles (naming the
resources), and hands each plugin the resources in an order that satisfies
every edge. What the plugin does next depends on how its tool handles order:

| Tool orders by | Targets | What `dependsOn` becomes |
|---|---|---|
| position in the file | `ansible`, `chef` | nothing written; the resources are emitted in order |
| declared relationships | `puppet`, `openvox`, `salt`, `dsc`, `terraform`, `opentofu`, `cloudformation`, `bicep`, `arm`, `pulumi`, `podman` | the tool's own keyword (`require`, `dependsOn`, `depends_on`, `DependsOn`, systemd's `Requires=` and `After=`). Salt also runs states in file order, so its states are written in order as well |
| its own rules | `docker` | `depends_on` between services, waiting for the other to be healthy when it declares a healthcheck; everything else Compose already creates first |
| the stages of a build | `packer` | the order of the provisioners, and post-processors chained into a sequence; a dependency against the fixed shape of a build is refused |
| nothing | `kubernetes` | the order of the manifests, with a warning that `kubectl` does not wait |
| the kind of each object | `helm` | nothing written where Helm's own install order already agrees; refused where it does not ([helm](../targets/helm.md#ordering)) |
| the stages of a first boot | `cloud-init` | nothing written; each resource is placed at the step cloud-init handles it, a file that has to wait is deferred, and a dependency that points backwards through those steps is refused |

Where a reference already implies order, as `${web_vm.id}` does in Terraform
or `Volume: ${data}:/srv` in a Podman unit, the plugin does not repeat it. ARM
is the exception: Resource Manager infers no order from a resource id, so the
`arm` target also writes a `dependsOn` for every reference.

## `notifies`: reaction

```yaml
- id: nginx_conf
  type: file
  params: {path: /etc/nginx/nginx.conf, content: "..."}
  notifies:
    - resource: nginx_svc
      action: restart
```

"When this actually changes, do that to the other." It is conditional: if the
file is already right, nothing restarts. It is not an order: Meridian leaves it
out of the sort, so add `dependsOn` if the notifier must also come first.

Tools differ most here, and Meridian is strict.

**Native** means the tool has its own mechanism. Ansible's handlers, Puppet's
and OpenVox's `notify`, Chef's `notifies`, Salt's `watch` and `onchanges` requisites,
DscPipelineRunner's `notify`, Compose's `depends_on` with `restart: true`, and
systemd's `PartOf=` for Podman units
([podman](../targets/podman.md#notifications)) are native.

**Synthetic** means the tool has no such mechanism, so Meridian builds the
reaction from something it does have. It works and is visible in the tool's
output, so each is documented on its target page:

- Kubernetes: a checksum of the notifier in an annotation on the workload's pod
  template. When the checksum changes, the pods roll.
  ([kubernetes](../targets/kubernetes.md#notifications))
- Helm: the same checksum, written as a template expression so Helm computes it
  each time it renders the chart. ([helm](../targets/helm.md#notifications))
- Docker: a checksum of a config, secret, network or volume in a label on the
  service. ([docker](../targets/docker.md#notifications))
- Terraform: `replace_triggered_by` on the notified resource, which replaces it.
  ([terraform](../targets/terraform.md#notifications))
- OpenTofu: the same `replace_triggered_by`.
  ([opentofu](../targets/opentofu.md#notifications))
- cloud-init: the notified action as a command that runs once, after every
  other first-boot command. A first boot changes everything, so it is
  unconditional. ([cloud-init](../targets/cloud-init.md#notifications))

A synthetic notify always produces a compile warning describing it.

**The action matters.** A target accepts only the actions its tool can actually
perform on that kind of resource. Puppet can restart
a service on refresh but cannot reload it, so `action: reload` is refused on
Puppet with the reason, not turned into a restart. Salt restarts and reloads a
service through the state that keeps it running, so either action aimed at a
service whose state is `stopped` is refused. Kubernetes and Helm can roll pods and
nothing else, so only `restart` aimed at a Deployment, StatefulSet or DaemonSet
is accepted. systemd can propagate a restart between Podman units and nothing
else, so only `restart` is accepted there. CloudFormation, Bicep, ARM templates and Pulumi cannot react to a change at
all and refuse every notify ([pulumi](../targets/pulumi.md#notifications) says
what to write instead). So does Packer: a build runs each step once on a
machine it then discards.

The [capability matrix](../reference/capability-matrix.md) summarises this. The
rule: Meridian does what the document says or refuses to compile it. See
[the capability model](capability-model.md).
