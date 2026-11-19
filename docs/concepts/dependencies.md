# Dependencies and notifications

`dependsOn` sets order. `notifies` sets a reaction to change.

## `dependsOn`: order

```yaml
- id: nginx_conf
  type: file
  params: {path: /etc/nginx/nginx.conf, content: "..."}
  dependsOn:
    - resource: nginx_pkg
```

Meridian sorts resources by every `dependsOn`, refuses cycles (naming the resources), and
hands each plugin the sorted list. What the plugin writes depends on the tool:

| Tool orders by | Targets | What `dependsOn` becomes |
|---|---|---|
| position in the file | `ansible`, `chef` | nothing written; the resources are emitted in order |
| declared relationships | `puppet`, `openvox`, `salt`, `dsc`, `terraform`, `opentofu`, `cloudformation`, `bicep`, `arm`, `pulumi`, `podman` | the tool's own keyword (`require`, `dependsOn`, `depends_on`, `DependsOn`, systemd's `Requires=` and `After=`). Salt also runs states in file order, so its states are written in order as well |
| its own rules | `docker` | `depends_on` between services, waiting for the other to be healthy when it declares a healthcheck; everything else Compose already creates first |
| the stages of a build | `packer` | the order of the provisioners, and post-processors chained into a sequence; a dependency against the fixed shape of a build is refused |
| nothing | `kubernetes` | the order of the manifests, with a warning that `kubectl` does not wait |
| the kind of each object | `helm` | nothing written where Helm's own install order already agrees; refused where it does not ([helm](../targets/helm.md#ordering)) |
| the stages of a first boot | `cloud-init` | nothing written; each resource is placed at the step cloud-init handles it, a file that has to wait is deferred, and a dependency that points backwards through those steps is refused |

Where a reference already implies order (`${web_vm.id}` in Terraform,
`Volume: ${data}:/srv` in a Podman unit), the plugin does not repeat it. The `arm` target
writes a `dependsOn` for every reference, since Resource Manager infers no order from an
id.

## `notifies`: reaction

```yaml
- id: nginx_conf
  type: file
  params: {path: /etc/nginx/nginx.conf, content: "..."}
  notifies:
    - resource: nginx_svc
      action: restart
```

When the notifier changes, the action runs on the other resource. If nothing changed,
nothing runs. A notify does not order the two; add `dependsOn` for that.

**Native** notifies use the tool's own mechanism: Ansible handlers, Puppet and OpenVox
`notify`, Chef `notifies`, Salt `watch` and `onchanges`, DscPipelineRunner `notify`,
Compose `depends_on` with `restart: true`, and systemd `PartOf=` for
[Podman](../targets/podman.md#notifications).

**Synthetic** notifies are built from something else the tool has, and always raise a
compile warning:

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
- cloud-init: the action as a command that runs once, after every other first-boot
  command, unconditionally. ([cloud-init](../targets/cloud-init.md#notifications))

**Meridian-run** notifies are performed by the plugin after the stage applies, when the
tool can do neither; see [below](#meridian-run-notifies).

**Accepted actions.** A target accepts only actions its tool can perform; anything else is
refused, never substituted:

- Puppet: `reload` is refused; refresh only restarts.
- Salt: an action on a `stopped` service is refused.
- Kubernetes and Helm: only `restart`, on a Deployment, StatefulSet or DaemonSet.
- Podman: only `restart`.
- CloudFormation, Bicep, ARM, Pulumi and Packer: every notify is refused
  ([pulumi](../targets/pulumi.md#notifications) says what to write instead).

## Meridian-run notifies

A plugin lists the actions it performs itself, by resource type (`acts`). Meridian takes
each notify it covers out of the document before compiling, with a compile warning, and
after the stage applies asks the plugin to perform the action
([decision 0056](../decisions/0056-meridian-runs-a-notify-the-tool-cannot-after-the-stage-applies.md)).
Native and synthetic come first: a plugin lists only what its tool cannot express.

| | |
|---|---|
| Trigger | the notifier was created, updated or replaced; not adopted or deleted |
| Timing | after the stage's apply, in apply order, each action once per stage |
| Plan | `would restart web (app_settings changes)`; `may` for every action when the tool cannot preview |
| Saved plan | the plan lists its actions; `apply --plan` runs those only, and fails the stage with `plan-differs` for any other it triggers |
| Pending | triggered actions are recorded in the run-state before any runs; one not run (failed apply, failed action, interrupt, not in the saved plan) stays pending and the next apply of the stage runs it |
| Failure | a failed action fails the stage, with the resource and `resource-failed`; later actions still run |
| Not covered | the tool's own plan and state never show the action; a rollback runs none, and a restart is not undone |

```text
web (ResourceSet, target fake on web1)
  warning: fake: page: Meridian runs touch on page after the stage applies, when config changes; the tool's own plan and state do not show it
  ~ update config: 2 bytes
  applied 1 change
  ran touch page: fake-tool touch files/page
```

`--output json` lists each stage's `actions` with `status` `would`, `may`, `done`,
`failed` or `pending`.

See the [capability matrix](../reference/capability-matrix.md).
