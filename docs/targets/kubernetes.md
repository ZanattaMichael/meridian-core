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

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) in the document goes in
one place: a value of a `v1/Secret`'s `stringData`. The cluster stores it as a Secret and a
workload reads it from there, by name, the way Kubernetes means it to.

```yaml
spec:
  target: kubernetes
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db_login
      type: v1/Secret
      params:
        stringData:
          username: app
          password: ${{ secret.db_password }}

    - id: db
      type: apps/v1/Deployment
      params:
        spec:
          template:
            spec:
              containers:
                - name: db
                  image: postgres:16
                  env:
                    - name: POSTGRES_PASSWORD
                      valueFrom:
                        secretKeyRef: {name: ${db_login}, key: password}
```

**What the compile writes.** The value is a token, `"${meridian_secret_db_password}"`,
inside the quotes of a string. The compiled `manifests.yaml` holds the token and never the
value, so a review of it sees exactly where the secret goes.

```yaml
stringData:
  password: "${meridian_secret_db_password}"
  username: app
```

**How the value reaches the tool.** A document with no secret is applied from the file,
`kubectl apply --filename manifests.yaml`, as before. One with a secret is applied from
standard input, `kubectl apply --filename -`: the runner fills each token in with its
value, escaped as a JSON string, on the way into `kubectl`, and the filled text is in
protected memory only. The plan's server-side dry run reads the same input. The command
line names no file and no value, and `kubectl`'s environment holds nothing of it.

**What the cluster keeps.** A Secret is stored in etcd base64-encoded, and encrypted at
rest only when the cluster is configured for it. Anyone who may `get secrets` in the
namespace reads it. An output that reads a Secret's `data` is marked sensitive and
masked wherever Meridian shows it.

**What the plan says.** A Secret written with `stringData` is planned as an update on
every run after the first, whether or not its values changed. The cluster stores
`stringData` as `data` and keeps no `stringData`, so the manifest's `stringData` is a
patch every time `kubectl apply` compares the two. That is kubectl's behaviour for any
Secret written this way, with or without Meridian; a document whose plan has to be in sync
can write a Secret's other fields in `data`, base64-encoded, but a secret's value cannot go
there, so a Secret that holds one is always reported as an update.

**What is refused.** A secret anywhere else is refused at compile, rule
`unsupported-secret-place`: in the `cluster` resource it would be on `kubectl`'s command
line; in any other object it would be written into the object for every reader to see;
in a Secret's `data` the document would have to base64-encode it itself, which turns a
secret into text; and inside a `stringData` value, such as `postgres://app:${{
secret.db_password }}@db/app`, because a value is one string, so the secret has to be the
whole value. Text that reads as a token, `${meridian_secret_`, is refused too, rule
`reserved-name`, so a document cannot make the runner fill a value in where the document
wrote the token itself.

The handover is tested against a stand-in `kubectl` that keeps what it reads
(`plugins/kubernetes/tests/run.rs`) and against a real `kubectl` and a kind cluster in CI
(`plugins/kubernetes/tests/real.rs`): the value is in the Secret the cluster holds, in no
file the plugin wrote, and in nothing the plan or apply prints.

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

## Editor schemas

`meridian schema sync` asks the cluster for its OpenAPI document (`kubectl
get --raw /openapi/v2`, against the `context` and `kubeconfig` the `cluster`
resource names), so an editor completes and checks the fields of every kind
the server knows, custom resources included
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | the `<apiVersion>/<Kind>` types the documents use: every field, `$ref`s followed, `status` marked as set by the cluster, `apiVersion` and `kind` left out because the type gives them |
| Listed by name | every other kind the server describes, with its description; `*List` kinds are left out |
| Pinned | the server's version, and the context |

A kind that the server does not describe, because its API group is not
installed, is reported in a note.

## Running

| | |
|---|---|
| Plan | `kubectl apply --dry-run=server --filename manifests.yaml`; with a [secret](#secrets) in the document, `--filename -` with the filled manifests on standard input |
| Apply | `kubectl apply --filename manifests.yaml`, or `--filename -` the same way, then each output with `kubectl get ... --output jsonpath=...` |
| `MERIDIAN_KUBECTL` | the program to run, when it is not `kubectl` on `PATH` |
| `MERIDIAN_KUBECTL_ARGS` | arguments added to every call, split like a shell command line (quotes hold a word together): `--context prod` |
| `MERIDIAN_KUBECTL_OUTPUT_WAIT` | seconds an apply waits for an output that has no value yet |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.kubernetes`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#kubernetes).

One limit of the plan: nothing is stored in a dry run, so objects that go in a
namespace the same document creates cannot be previewed until the namespace
exists. Meridian reports that plan as unknown instead of failing it.

The runner is tested against a real `kubectl` and a [kind](https://kind.sigs.k8s.io)
cluster on every pull request that touches it: a plan against an empty cluster,
an apply checked with `kubectl get`, a second plan that is in sync, a changed
ConfigMap that restarts the Deployment it notifies, and a manifest the API
server refuses. A managed cluster and admission webhooks have not been run this
way. See [Real-tool tests](../contributing/real-tool-tests.md).
