# Target: `kubernetes`

<img src="../branding/logos/meridian-kubernetes.svg" width="72" alt="Meridian Kubernetes plugin logo">

Compiles an Infrastructure document into Kubernetes manifests and applies them
with `kubectl`.

## What it does and does not support

| | |
|---|---|
| Resource types | Kubernetes' own: API version and kind, such as `apps/v1/Deployment` or `v1/Service`, plus `cluster` for where the manifests go. Custom resources work the same way |
| `dependsOn` | Orders the manifests in the file, and nothing more. `kubectl` applies them in that order without waiting for one to be ready. A warning says so |
| `notifies` | Synthetic: the action `restart`, aimed at a Deployment, StatefulSet or DaemonSet. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused |
| `state: absent` | No. Applying manifests never deletes an object; delete it with `kubectl` and remove it from the document |
| Plan | Yes: a server-side dry run |
| Outputs | Yes: any path into a live object |

## What a document looks like

A resource's `type` is the object's API version and kind and its `params` are
the object's fields, exactly as Kubernetes documents them: `metadata`, `spec`,
`data`. `apiVersion` and `kind` come from the type and cannot be set again.

An object with no `metadata.name` is named after the resource's id, with
underscores turned into hyphens (`web_config` becomes `web-config`).

One resource may have the type `cluster`. It is not a manifest:

| Param | Meaning |
|---|---|
| `context` | the kubeconfig context to use |
| `namespace` | the namespace for objects that do not set their own |
| `kubeconfig` | a kubeconfig file other than the default |

## References

Kubernetes does not wire one object's state into another's spec, so inside
`params` a reference can only be something known at compile time:

| You write | You get |
|---|---|
| `${web_config}` or `${web_config.metadata.name}` | the object's name |
| `${web_config.metadata.namespace}` | its namespace |

Anything else, such as `${web_svc.status.loadBalancer.ingress[0].ip}`, is
refused in a param with an explanation. It can be read in an output.

## Outputs

An output is read from the cluster after the apply, so it can be any path into
the live object:

```yaml
outputs:
  - name: web_url
    value: http://${web_svc.status.loadBalancer.ingress[0].ip}
```

A load balancer's address takes a while to appear. After an apply Meridian waits
for an output that has no value yet, two minutes by default
(`MERIDIAN_KUBECTL_OUTPUT_WAIT`, in seconds). An output that never arrives is an
error, not an empty value. Anything read from a Secret is reported as sensitive.

## Notifications

Kubernetes does not restart pods when a ConfigMap or Secret they use changes.
For a notify with the action `restart`, Meridian writes a checksum of the
notifier into an annotation on the workload's pod template,
`meridian.dev/restart-on-<name>`. When the notifier's content changes, the
template changes, and the workload's own controller rolls its pods. The compile
warns that this is how it works, and the annotation is visible in the manifest
and on the pods.

## Example

[`examples/kubernetes/web.yaml`](../../examples/kubernetes/web.yaml) declares a
ConfigMap, a Deployment and a Service, and compiles to
[`web.manifests_yaml`](../../plugins/kubernetes/testdata/web.manifests_yaml). The
ConfigMap and the part of the Deployment it touches:

```yaml
- id: web_config
  type: v1/ConfigMap
  params:
    data:
      nginx.conf: |
        server { ... }
  notifies:
    - resource: web
      action: restart
```

```yaml
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: web-config
data:
  nginx.conf: |
    server { ... }
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  template:
    metadata:
      annotations:
        meridian.dev/restart-on-web-config: 3e7aa8895791e35c
```

## Importing

`meridian import --from kubernetes PATHS` reads manifests, in YAML or JSON, and writes
one Infrastructure document holding every object in them. [Importing existing configuration](../operations/importing.md) is the guide.

A manifest is what this target compiles to, so the mapping is the target's own, read
backwards:

| In the manifests | In the document |
|---|---|
| An object | a resource whose type is `apiVersion/kind` and whose params are the object's other fields |
| The object's name | the resource's id, with hyphens turned into underscores. `metadata.name` is left out where the id gives the same name back |
| A second object of the same name, such as a Deployment and its Service | the id `<name>_<kind>`. The object keeps its own name |
| A `List`, as `kubectl get -o yaml` prints several objects | its items, one resource each |
| Several documents in one file, and several files | every object, in the order `kubectl apply` would have sent them |
| `${VAR}` in a string, as in a shell script held in a ConfigMap | written `$${VAR}`, so the document does not read it as a reference and the compiled manifest has `${VAR}` again |

The files are read by YAML 1.1's rules, as Kubernetes reads them: `yes` and `on` are
booleans, and `0644` is an octal number.

What is reported:

| In the manifests | What happens | Level |
|---|---|---|
| `status`, and what a cluster writes into `metadata`: `uid`, `resourceVersion`, `creationTimestamp`, `generation`, `managedFields`, `selfLink`, and the `last-applied-configuration` and `deployment.kubernetes.io/revision` annotations | Left out. None of it is desired state, and `kubectl apply` refuses some of it. This is what makes an object exported from a cluster importable | note |
| An object with `generateName` and no name | Named after its id, because `kubectl apply` needs a name | changed |
| A `Kustomization` | Left out. It is instructions for kustomize: build it with `kubectl kustomize` and import what that prints, or use the [helm](helm.md) target, which takes an overlay | not translated |
| A YAML document that is not an object | Left out | not translated |

A Helm template is refused outright, with the advice to render the chart with
`helm template` and import what it prints. A manifest that merely holds `{{ }}` inside a
string, as a Prometheus rule does, is not mistaken for one.

The round-trip test imports
[`web/web.yaml`](../../plugins/kubernetes/testdata/import/web/web.yaml), compiles the
document, and compares the manifests it compiles to with the originals, object for
object. The imported document is
[`web.meridian.yaml`](../../plugins/kubernetes/testdata/import/web.meridian.yaml).

## Running

| | |
|---|---|
| Plan | `kubectl apply --dry-run=server --filename manifests.yaml` |
| Apply | `kubectl apply --filename manifests.yaml`, then each output with `kubectl get ... --output jsonpath=...` |
| `MERIDIAN_KUBECTL` | the program to run, when it is not `kubectl` on `PATH` |
| `MERIDIAN_KUBECTL_ARGS` | arguments added to every call |
| `MERIDIAN_KUBECTL_OUTPUT_WAIT` | seconds an apply waits for an output that has no value yet |

One limit of the plan: nothing is stored in a dry run, so objects that go in a
namespace the same document creates cannot be previewed until the namespace
exists. Meridian reports that plan as unknown instead of failing it.

The runner is tested against a real `kubectl` and a [kind](https://kind.sigs.k8s.io)
cluster on every pull request that touches it: a plan against an empty cluster,
an apply checked with `kubectl get`, a second plan that is in sync, a changed
ConfigMap that restarts the Deployment it notifies, and a manifest the API
server refuses. A managed cluster and admission webhooks have not been run this
way. See [Real-tool tests](../contributing/real-tool-tests.md).
