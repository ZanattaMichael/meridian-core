# Target: `helm`

<img src="../branding/logos/meridian-helm.svg" width="72" alt="Meridian Helm plugin logo">

Compiles an Infrastructure document of Kubernetes objects into a Helm chart and
installs it as a release with `helm`. A document may also carry a Kustomize
overlay, which is applied to what the chart renders before Helm installs it.

## What it does and does not support

| | |
|---|---|
| Resource types | Kubernetes' own: API version and kind, such as `apps/v1/Deployment` or `v1/Service`, custom resources included. Plus four settings types, at most one of each: `values`, `chart`, `release` and `kustomization`. See [Resource types](#resource-types) |
| References | Chart values, the release, the chart, and the name and namespace of another object. Each becomes a Helm template action. See [References](#references) |
| Kustomize overlay | Yes, with inline patches. An overlay cannot name other files. See [Kustomize overlays](#kustomize-overlays) |
| `dependsOn` | Where Helm's own install order agrees; otherwise refused, or with `ordering: releases` one release per wave. See [Ordering](#ordering) |
| `notifies` | Synthetic: the action `restart`, aimed at a Deployment, StatefulSet or DaemonSet. Any other action is refused. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused |
| `adopt` | No. Refused: Helm refuses an object that no release of its own owns |
| `state: absent` | No. Refused. Remove the resource; Helm deletes what the chart no longer declares |
| Plan | Yes: what would be installed, compared with what Helm last installed. It does not detect changes made to the cluster behind Helm's back |
| Apply | Yes: `helm upgrade --install`. An unchanged document adds no revision |
| Outputs | Yes: anything a template can read, and any path into a live object, read with `kubectl` |
| Destroy | Yes: `helm uninstall`. See [Destroying](../operations/destroying.md) |
| Subcharts, hand-written hooks, `lookup` | No. The chart holds only what the document declares |
| Editor schemas | Yes: `meridian schema sync` asks the cluster what each kind's fields are, as the kubernetes target does. See [Editor schemas](#editor-schemas) |

What is missing fails at compile ([Compile errors](#compile-errors)).

| | |
|---|---|
| Category | `container-orchestration` |
| Document kind | `Infrastructure` |
| Tools it runs | `helm`; `kustomize` when the document has an overlay; `kubectl` when an output reads from the cluster |

### `helm` or `kubernetes`?

Both take the same objects; switch with `spec.target` and the settings resource.

| | `helm` | [`kubernetes`](kubernetes.md) |
|---|---|---|
| What is installed | One Helm release, with history and rollback | Plain manifests, with `kubectl apply` |
| Removing a resource from the document | Helm deletes the object | The object stays until you delete it |
| Values and overlays | Yes | No |
| `dependsOn` | Where Helm's order agrees, or one release per wave | Applied in waves, waiting for each |
| Plan | Against the last release | A server-side dry run |

## What a document looks like

This is [`examples/helm/web-app.yaml`](../../examples/helm/web-app.yaml), shortened.

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web-app
spec:
  target: helm
  resources:
    - id: values
      type: values
      params:
        replicas: 2
        image:
          repository: nginx
          tag: "1.27"

    - id: release
      type: release
      params:
        namespace: web
        createNamespace: true
        wait: true

    - id: web_config
      type: v1/ConfigMap
      params:
        metadata:
          name: ${release.name}-config
        data:
          index.html: "<h1>Hello</h1>"
      notifies:
        - resource: web
          action: restart

    - id: web
      type: apps/v1/Deployment
      params:
        metadata:
          name: ${release.name}
        spec:
          replicas: ${values.replicas}
          selector:
            matchLabels: {app: web}
          template:
            metadata:
              labels: {app: web}
            spec:
              containers:
                - name: nginx
                  image: ${values.image.repository}:${values.image.tag}
                  volumeMounts:
                    - name: content
                      mountPath: /usr/share/nginx/html
              volumes:
                - name: content
                  configMap:
                    name: ${web_config}

    - id: web_svc
      type: v1/Service
      params:
        metadata:
          name: ${release.name}
        spec:
          type: LoadBalancer
          selector: {app: web}
          ports:
            - port: 80

  outputs:
    - name: web_address
      value: ${web_svc.status.loadBalancer.ingress[0].ip}
```

```console
$ meridian compile web-app.yaml     # write the chart, change nothing
$ meridian plan web-app.yaml        # what an apply would create, update and delete
$ meridian apply web-app.yaml       # install or upgrade the release, report the outputs
```

## Resource types

### Kubernetes objects

`type` is `apiVersion/kind` (`v1/Service`, `apps/v1/Deployment`,
`cert-manager.io/v1/Certificate`); `params` are the rest of the manifest, without
`apiVersion` or `kind`. Without `metadata.name`, the id is used with `_` as `-`. Each object
becomes `templates/<id>.yaml`. Same as the `kubernetes` target.

### `values`

Default values, written to `values.yaml`. Literal; no references.

```yaml
- id: values
  type: values
  params:
    replicas: 2
    image: {repository: nginx, tag: "1.27"}
```

Quote versions: `tag: 1.20` is the number 1.2.

### `chart`

Chart metadata; all optional.

| Param | Default | Meaning |
|---|---|---|
| `name` | the document's `metadata.name` | The chart's name: lowercase letters, digits and hyphens. |
| `version` | `0.1.0` | A semantic version, in quotes if it could be read as a number. |
| `appVersion` | none | The version of what the chart deploys. |
| `description` | "Compiled by Meridian from the document …" | One line about the chart. |

### `release`

Install settings; all optional.

| Param | Default | Meaning |
|---|---|---|
| `name` | the document's `metadata.name` | The release name: lowercase letters, digits and hyphens, at most 53 characters. |
| `namespace` | `default` | The namespace the release is installed into. |
| `createNamespace` | `false` | Create that namespace if it is not there (`--create-namespace`). |
| `wait` | `false` | Wait until the release's workloads are ready (`--wait`). |
| `atomic` | `false` | Roll back a failed install or upgrade (`--atomic`). |
| `timeout` | Helm's own | How long to wait, as a duration: `90s`, `5m`, `1h30m`. |
| `ordering` | `kind` | `kind`: Helm's own order. `releases`: one release per `dependsOn` wave. See [Ordering](#ordering). |

### `kustomization`

A Kustomize overlay. See [Kustomize overlays](#kustomize-overlays).

At most one of each of these four.

## References

`${id}` or `${id.path}`. `$${` is a literal `${`.

| Written | Refers to | In the chart |
|---|---|---|
| `${values.image.tag}` | A value, by path. `values` is the id of the `values` resource. | `{{ .Values.image.tag \| toRawJson }}` |
| `${values.hosts[0]}`, `${values["my-key"]}` | A list item, or a key that is not a plain word. | `{{ index … }}` |
| `${release.name}`, `${release.namespace}` | The release. `release` is the id of the `release` resource. | `{{ .Release.Name \| toRawJson }}`, `{{ .Release.Namespace \| toRawJson }}` |
| `${chart.name}`, `${chart.version}`, `${chart.appVersion}` | The chart. | `{{ .Chart.Name \| toRawJson }}` and so on |
| `${web_config}` or `${web_config.metadata.name}` | The name of another object. | That object's name, as its own manifest writes it |
| `${web_config.metadata.namespace}` | The namespace another object lands in. | Its own, or `{{ .Release.Namespace }}` |

- Every action ends in `toRawJson`, so types survive.
- A missing value is a compile error.
- A whole-value reference keeps its type; inside text it makes text
  (`{{ printf "%s:%s" .Values.image.repository .Values.image.tag | toRawJson }}`).
- Cluster state such as `status` is read in an [output](#outputs).

### Text is never a template

`params` reach the cluster as written; a `{{` is never interpreted by Helm.

```yaml
data:
  alert.tmpl: "{{ .Labels.instance }} is down"   # arrives exactly like this
```

## Notifications

`notifies` compiles to a checksum of the notifier on the pod template.

```yaml
- id: web_config
  type: v1/ConfigMap
  params: {data: {mode: fast}}
  notifies:
    - resource: web
      action: restart
```

The Deployment `web` gets this on its pod template:

```yaml
annotations:
  checksum/web_config: {{ include (print $.Template.BasePath "/web_config.yaml") . | sha256sum | quote }}
```

A change rolls the pods. The compile warns, and a plan shows the workload updated with the
notifier. Any kind may notify. Refused (`unsupported-action`):

- actions other than `restart`;
- targets other than a Deployment, StatefulSet or DaemonSet;
- notify cycles;
- non-mapping `spec.template.metadata.annotations`.

Notifies to or from `values`, `chart`, `release` or `kustomization` are refused
(`unsupported-dependency`).

## Ordering

Helm applies objects in one pass, sorted by kind:

PriorityClass, Namespace, NetworkPolicy, ResourceQuota, LimitRange, PodSecurityPolicy,
PodDisruptionBudget, ServiceAccount, Secret, ConfigMap, StorageClass, PersistentVolume,
PersistentVolumeClaim, CustomResourceDefinition, ClusterRole, ClusterRoleBinding, Role,
RoleBinding, Service, DaemonSet, Pod, ReplicationController, ReplicaSet, Deployment,
HorizontalPodAutoscaler, StatefulSet, Job, CronJob, IngressClass, Ingress, APIService,
and last every kind Helm does not know, custom resources among them.

`dependsOn` is accepted where this order honours it, and writes nothing. Otherwise, or
between the same kind, it is refused (`unsupported-dependency`); Kubernetes retries, so it
can usually be removed. `wait: true` on the release waits for readiness.

### Ordered releases

With `ordering: releases` on the release, a `dependsOn` Helm's order does not honour splits
the chart into one release per wave: an object goes one wave after the latest object it
depends on. Meridian installs `<release>-1`, `<release>-2`, ... in order, each with `--wait`,
and uninstalls them last first
([decision 0057](../decisions/0057-kubernetes-applies-depends-on-in-waves-and-waits-between.md)).
The compile warns.

```yaml
- id: release
  type: release
  params: {name: shop, ordering: releases}
- id: db
  type: apps/v1/StatefulSet
  params: {...}
- id: api
  type: apps/v1/Deployment
  params: {...}
  dependsOn:
    - resource: db        # shop-1 holds db, shop-2 holds api
```

| Trade-off | Effect |
|---|---|
| Release identity | `helm list` shows each release; `helm rollback` of one does not roll back the others |
| A document installed as one release | Refused until it is uninstalled: moving objects between releases needs `adopt` ([#269](https://github.com/ZanattaMichael/meridian-core/issues/269)) |
| A notify between two releases | Refused (`unsupported-action`): the checksum reads a template of the same chart |
| A Kustomize overlay | Refused (`unsupported-dependency`) |

A `dependsOn` Helm's order already honours keeps one release.

## Kustomize overlays

A `kustomization` resource's params are `kustomization.yaml` fields.
[`examples/helm/web-app-kustomize.yaml`](../../examples/helm/web-app-kustomize.yaml),
shortened:

```yaml
- id: overlay
  type: kustomization
  params:
    namePrefix: prod-
    labels:
      - pairs: {environment: production}
    images:
      - name: nginx
        newName: registry.example.com/mirror/nginx
    replicas:
      - name: ${web}
        count: 4
    patches:
      - target: {kind: Deployment, name: "${web}"}
        patch:
          - op: add
            path: /spec/template/spec/containers/0/resources
            value: {limits: {memory: 256Mi}}
```

Patches are written as YAML (or a string).

### How the two tools are combined

1. Helm renders the chart (`helm template`).
2. Kustomize applies the overlay to what Helm rendered (`kustomize build`).
3. Helm installs the patched objects as the release.

It is one release; `helm history`, `rollback` and `get manifest` work as usual.
`--post-renderer` is not used. Tested only with the Helm on GitHub's runners.

### What an overlay cannot do

Fields naming other files are refused: `resources`, `bases`, `components`, `crds`,
`configurations`, `generators`, `transformers`, `validators`, `helmCharts`, `helmGlobals`,
`openapi`, and a patch's `path`.

References in an overlay are settled at compile (`${web}` is the pre-overlay name).
`namePrefix`, `nameSuffix` and `namespace` are followed in reports; other renames show the
tool's name.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) goes only in `values`,
read with `${values.<path>}` into a `v1/Secret`'s `stringData`.

```yaml
spec:
  target: helm
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: values
      type: values
      params:
        dbPassword: ${{ secret.db_password }}

    - id: db_login
      type: v1/Secret
      params:
        stringData:
          password: ${values.dbPassword}
```

`chart/values.yaml` holds a token, `"${meridian_secret_db_password}"`. `helm template` and
`helm upgrade --install` get `--values -`; the runner fills in tokens on the way in, from
protected memory.

Helm stores values in a release Secret (base64); `helm get values` prints them. Outputs read
from a Secret's `data` are masked.

Refused (`unsupported-secret-place`): a secret in `chart`, `release` or `kustomization`, in
an object's params, or in the values of a document with an
[overlay](#kustomize-overlays). A literal `${meridian_secret_` is refused (`reserved-name`).

Tested against a stand-in `helm` (`plugins/helm/tests/run.rs`) and real `helm` in CI
(`plugins/helm/tests/real.rs`).

## Outputs

An output may hold:

- what a template can read: `${release.name}`, `${values.port}`, `${web_svc}`;
- a path into a live object, read with `kubectl` after apply:
  `${web_svc.status.loadBalancer.ingress[0].ip}`,
  `${web.status.readyReplicas}`,
  `${web.metadata.annotations["example.com/owner"]}`.

```yaml
outputs:
  - name: web_url
    value: http://${web_svc.status.loadBalancer.ingress[0].ip}/
  - name: ready
    value: ${web.status.readyReplicas}
```

A single reference keeps its type. Secret values are masked, still base64. For values the
cluster fills in later, set `wait: true`.

## Example

From [`examples/helm/web-app.yaml`](../../examples/helm/web-app.yaml):

```yaml
- id: web_config
  type: v1/ConfigMap
  params:
    metadata:
      name: ${release.name}-config
    data:
      index.html: |
        <h1>${values.greeting}</h1>
      banner.tmpl: "{{ .Site.Title }} is served from port ${values.port}"
  notifies:
    - resource: web
      action: restart
```

compile to
[`chart/templates/web_config.yaml`](../../plugins/helm/testdata/web-app.chart_templates_web_config_yaml):

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ printf "%s-config" .Release.Name | toRawJson }}
data:
  banner.tmpl: {{ printf "{{ .Site.Title }} is served from port %s" (toRawJson .Values.port) | toRawJson }}
  index.html: {{ printf "<h1>%s</h1>\n" .Values.greeting | toRawJson }}
```

and, in
[`chart/templates/web.yaml`](../../plugins/helm/testdata/web-app.chart_templates_web_yaml):

```yaml
spec:
  replicas: {{ .Values.replicas | toRawJson }}
  template:
    metadata:
      annotations:
        checksum/web_config: {{ include (print $.Template.BasePath "/web_config.yaml") . | sha256sum | quote }}
```

with this warning:

```
helm: web: restarts by rolling its pods whenever "web_config" changes: Helm has no handlers, so a checksum of that object is kept on the pod template as the annotation checksum/web_config
```

[`examples/helm/web-app-kustomize.yaml`](../../examples/helm/web-app-kustomize.yaml)
adds an overlay. Compiled files are in [`plugins/helm/testdata`](../../plugins/helm/testdata).

## What a compile writes

```
chart/Chart.yaml
chart/values.yaml
chart/templates/<id>.yaml      one per object
kustomize/kustomization.yaml   only with an overlay
release.json                   for Meridian's runner; Helm never reads it
```

`chart/` is an ordinary chart that passes `helm lint`. `release.json` holds release
settings, object mapping and outputs.

## Editor schemas

As on [kubernetes](kubernetes.md#editor-schemas), through this target's `kubectl`
settings (`--context` in `MERIDIAN_KUBECTL_ARGS`) ([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | the `<apiVersion>/<Kind>` types the documents use, as for kubernetes |
| Listed by name | every other kind the server describes |
| Pinned | the server's version |

The plugin describes `chart`, `release`, `values` and `kustomization` itself.

## Running

| Step | Command |
|---|---|
| Render | `helm template <release> chart --namespace <ns>` |
| Overlay, if any | `kustomize build kustomize` |
| Read the release | `helm status <release> --output json`, `helm get manifest <release>` |
| Apply | `helm upgrade --install <release> chart --namespace <ns>` plus the release's settings |
| Outputs | `kubectl get <kind> <name> --namespace <ns> --output json` |
| Destroy | `helm status`, `helm get manifest` (what goes), then `helm uninstall <release> --wait`, with the release's `--timeout`. A release that is not installed has nothing to remove. The namespace is kept |

**Plan** compares with the last release's manifest. It does not see out-of-band changes.

**Apply** skips a matching deployed release, adding no revision. A failed release is
upgraded again; a plan shows it as "may change".

[`--tool-debug`](../reference/cli.md#asking-a-tool-for-more-output) adds `--debug` to `helm` at every level.

### Reading a failure

When `helm upgrade` or `helm uninstall` fails, each object Helm names is a
[resource failure](../reference/state.md#resource-failures) on its id. With `wait: true`,
`resource Deployment/shop/web not ready. status: InProgress` is `timed-out`, and
`status: Failed` is `resource-failed`, with the status as the tool code; a wait that names
nothing is `timed-out` on the release. Refusals by the API server are read as the
[Kubernetes target](kubernetes.md#reading-a-failure) reads them.

## Configuration

Environment variables.

| Variable | Default | Meaning |
|---|---|---|
| `MERIDIAN_HELM` | `helm` | The Helm program. |
| `MERIDIAN_HELM_ARGS` | none | Arguments added to every `helm` call: `--kube-context prod`. |
| `MERIDIAN_KUSTOMIZE` | `kustomize build` | The command that builds an overlay. `kubectl kustomize` uses the copy built into kubectl. |
| `MERIDIAN_KUBECTL` | `kubectl` | The program that reads outputs. |
| `MERIDIAN_KUBECTL_ARGS` | none | Arguments added to every `kubectl` call: `--context prod`. |

Settings can also go in `meridian.yaml` under `spec.settings.helm`; the environment wins.
See the [plugin settings reference](../reference/plugin-settings.md#helm). Helm and kubectl
use your kubeconfig or `KUBECONFIG`; point both at the same cluster.

## Compile errors

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | The type is not `apiVersion/Kind` or a settings type. | Write `apps/v1/Deployment`, not `Deployment`. |
| `invalid-reference` | A reference names nothing, reads a value that does not exist, or reads live state inside a template. | Fix the name or the path. Read live state in an output. |
| `invalid-param` | A setting has the wrong type or form, or `apiVersion` or `kind` is among the params. | The message names the param. |
| `unsupported-param` | A settings resource has a param the target does not know, an overlay names a file, or an overlay sets `apiVersion` or `kind`, which Meridian writes. | The message lists what is accepted. |
| `invalid-name` | An id cannot name a template, or a name cannot be a Kubernetes name. | Use letters, digits, `_` and `-` in ids; set `metadata.name`. |
| `invalid-name` | The document's name cannot be used as it is. | Add a `release` or `chart` resource with a `name`. |
| `duplicate-resource` | Two resources of one settings type. | Keep one. |
| `duplicate-resource` | Two resources are the same object. | Give one a different `metadata.name`. |
| `duplicate-output` | Two outputs share a name. | Rename one. |
| `empty-document` | The document has no Kubernetes object. | Add one. |
| `unsupported-dependency` | A `dependsOn` Helm's install order does not honour, or one on a settings resource; `ordering: releases` with a `kustomization`. | Remove it, or set `ordering: releases`; see [Ordering](#ordering). |
| `unsupported-action` | A notification other than a restart of a workload, or two objects that notify each other. | See [Notifications](#notifications). |
| `unsupported-state`, `unsupported-runtime-condition` | `state: absent` or `runtimeWhen`. | Remove the resource or the condition. |
| `unsupported-secret-place` | A secret anywhere but the `values`, or in the values of a document with a Kustomize overlay. | See [Secrets](#secrets). |
| `reserved-name` | A value holds the text `${meridian_secret_`, which is how a secret's place is written. | Write the secret as `${{ secret.<name> }}`, or choose other text. |

## Run errors

| Message | Meaning |
|---|---|
| `could not run helm: it is not installed…` | The tool is not on the `PATH`. Install it or set `MERIDIAN_HELM`. The same goes for `kustomize` and `kubectl`. |
| `helm could not render the chart` | Helm rejected the chart. The output that follows is Helm's own. |
| `kustomize could not apply the overlay` | Kustomize rejected the overlay, most often a patch whose target matches nothing. |
| `helm could not install the release…` | The install or upgrade failed. `helm status` and `helm history` show the state it was left in. |
| `output "x": the Service web has no status.loadBalancer.ingress yet` | The cluster has not filled the value in. Set `wait: true` on the release. |
| `nothing has been applied for this document yet, so it has no outputs` | Outputs were asked for before the first apply. |

## How it is tested

- Unit tests, and golden files in [`plugins/helm/testdata`](../../plugins/helm/testdata).
- Stand-ins for `helm`, `kustomize` and `kubectl`.
- Real `helm`, `kustomize` and a `kind` cluster in the `helm` workflow. Locally:

```console
$ cargo test -p meridian-target-helm --test real -- --ignored
$ cargo test -p meridian-target-helm --test cluster -- --ignored
```

## Inspecting one resource

[`meridian inspect`](../reference/cli.md#inspect) renders the chart as a plan does.

| Resource | Actual | As declared |
|---|---|---|
| an object of the chart | `kubectl get KIND/NAME --output json`, cut to the fields the rendered chart declares; a Secret's values are `(sensitive)` | the release is `deployed` and `helm get manifest` holds the object as rendered |
| the release, chart or values | `helm status`: the release's `status` | the release is `deployed` and holds every object as rendered |
