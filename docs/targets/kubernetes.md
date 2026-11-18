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

`type` is `apiVersion/kind`; `params` are the object's fields (`metadata`, `spec`, `data`),
without `apiVersion` or `kind`. Without `metadata.name`, the id is used with `_` as `-`.

An optional `cluster` resource is not a manifest:

| Param | Meaning |
|---|---|
| `context` | the kubeconfig context to use |
| `namespace` | the namespace for objects that do not set their own |
| `kubeconfig` | a kubeconfig file other than the default |

## References

In `params`, only compile-time values:

| You write | You get |
|---|---|
| `${web_config}` or `${web_config.metadata.name}` | the object's name |
| `${web_config.metadata.namespace}` | its namespace |

Anything else, such as `${web_svc.status.loadBalancer.ingress[0].ip}`, is refused; read it
in an output.

## Outputs

Outputs are read after apply, from any path in the live object:

```yaml
outputs:
  - name: web_url
    value: http://${web_svc.status.loadBalancer.ingress[0].ip}
```

Meridian waits for a missing value, 120 seconds by default
(`MERIDIAN_KUBECTL_OUTPUT_WAIT`); one that never arrives is an error. Secret values are
sensitive.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) goes only in a
`v1/Secret`'s `stringData` value.

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

`manifests.yaml` holds a token, `"${meridian_secret_db_password}"`.

```yaml
stringData:
  password: "${meridian_secret_db_password}"
  username: app
```

With a secret, the document is applied with `kubectl apply --filename -`; the runner fills
in tokens on the way in, from protected memory. The dry-run plan reads the same input.

etcd stores Secrets base64-encoded, encrypted only if the cluster is configured so. Outputs
from a Secret's `data` are masked.

A Secret with `stringData` plans as an update on every run; that is kubectl's behaviour.

Refused (`unsupported-secret-place`): a secret in `cluster`, in any other object, in a
Secret's `data`, or inside a longer `stringData` value. A literal `${meridian_secret_` is
refused (`reserved-name`).

Tested against a stand-in `kubectl` (`plugins/kubernetes/tests/run.rs`) and a kind cluster
in CI (`plugins/kubernetes/tests/real.rs`).

## Notifications

A `restart` notify writes a checksum of the notifier to the pod template annotation
`meridian.dev/restart-on-<name>`, so a change rolls the pods. The compile warns.

## Example

[`examples/kubernetes/web.yaml`](../../examples/kubernetes/web.yaml) compiles to
[`web.manifests_yaml`](../../plugins/kubernetes/testdata/web.manifests_yaml):

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

`meridian import --from kubernetes PATHS` reads YAML or JSON manifests into one document
([guide](../operations/importing.md)). The mapping is the compile's, reversed:

| In the manifests | In the document |
|---|---|
| An object | a resource whose type is `apiVersion/kind` and whose params are the object's other fields |
| The object's name | the resource's id, with hyphens turned into underscores. `metadata.name` is left out where the id gives the same name back |
| A second object of the same name, such as a Deployment and its Service | the id `<name>_<kind>`. The object keeps its own name |
| A `List`, as `kubectl get -o yaml` prints several objects | its items, one resource each |
| Several documents in one file, and several files | every object, in the order `kubectl apply` would have sent them |
| `${VAR}` in a string, as in a shell script held in a ConfigMap | written `$${VAR}`, so the document does not read it as a reference and the compiled manifest has `${VAR}` again |

Files are read as YAML 1.1. Reported:

| In the manifests | What happens | Level |
|---|---|---|
| `status`, and what a cluster writes into `metadata`: `uid`, `resourceVersion`, `creationTimestamp`, `generation`, `managedFields`, `selfLink`, and the `last-applied-configuration` and `deployment.kubernetes.io/revision` annotations | Left out. None of it is desired state, and `kubectl apply` refuses some of it. This is what makes an object exported from a cluster importable | note |
| An object with `generateName` and no name | Named after its id, because `kubectl apply` needs a name | changed |
| A `Kustomization` | Left out. It is instructions for kustomize: build it with `kubectl kustomize` and import what that prints, or use the [helm](helm.md) target, which takes an overlay | not translated |
| A YAML document that is not an object | Left out | not translated |

A Helm template is refused; import `helm template` output. `{{ }}` inside a string is
fine. Round-trip test:
[`web/web.yaml`](../../plugins/kubernetes/testdata/import/web/web.yaml) and
[`web.meridian.yaml`](../../plugins/kubernetes/testdata/import/web.meridian.yaml).

## Editor schemas

`meridian schema sync` reads `kubectl get --raw /openapi/v2` with the `cluster` resource's
`context` and `kubeconfig`, custom resources included
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | the `<apiVersion>/<Kind>` types the documents use: every field, `$ref`s followed, `status` marked as set by the cluster, `apiVersion` and `kind` left out because the type gives them |
| Listed by name | every other kind the server describes, with its description; `*List` kinds are left out |
| Pinned | the server's version, and the context |

Undescribed kinds are noted.

## Running

| | |
|---|---|
| Plan | `kubectl apply --dry-run=server --filename manifests.yaml`; with a [secret](#secrets) in the document, `--filename -` with the filled manifests on standard input |
| Apply | `kubectl apply --filename manifests.yaml`, or `--filename -` the same way, then each output with `kubectl get ... --output jsonpath=...` |
| `MERIDIAN_KUBECTL` | the program to run, when it is not `kubectl` on `PATH` |
| `MERIDIAN_KUBECTL_ARGS` | arguments added to every call, split like a shell command line (quotes hold a word together): `--context prod` |
| `MERIDIAN_KUBECTL_OUTPUT_WAIT` | seconds an apply waits for an output that has no value yet |

Settings can also go in `meridian.yaml` under `spec.settings.kubernetes`; the environment
wins. See the [plugin settings reference](../reference/plugin-settings.md#kubernetes).

Objects in a namespace the document creates cannot be dry-run first; that plan is unknown.

CI tests a real `kubectl` and [kind](https://kind.sigs.k8s.io) cluster: plan, apply,
in-sync, notify restart and a refused manifest. Managed clusters and admission webhooks are
untested. See [Real-tool tests](../contributing/real-tool-tests.md).
