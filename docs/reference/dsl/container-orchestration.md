# The container orchestration flavour

How the [Meridian language](index.md) is written for tools that hand objects to a
cluster.

| | |
|---|---|
| Category, as `meridian plugins` prints it | `container-orchestration` |
| Kind of document | `Infrastructure`. A `ResourceSet` is refused: a cluster is not one machine |
| `type` and `params` | the cluster's own API, passed through |
| Outputs | yes |

## Targets

| Target | `type` is | Example | Settings resource | Page |
|---|---|---|---|---|
| `kubernetes` | API version and kind | `apps/v1/Deployment`, `v1/Service` | `cluster` (optional) | [kubernetes](../../targets/kubernetes.md) |
| `helm` | API version and kind, the same as `kubernetes` | `apps/v1/Deployment`, `v1/Service` | `values`, `chart`, `release`, `kustomization` (each optional, at most one) | [helm](../../targets/helm.md) |

## Two ways to deliver the same objects

Both take the same objects; a document moves between them by changing `spec.target` and
its settings resources.

| | `kubernetes` | `helm` |
|---|---|---|
| Compiles to | plain manifests | a Helm chart |
| Installed with | `kubectl apply` | `helm upgrade --install`, as one release with history and rollback |
| Removing a resource from the document | the object stays until you delete it | Helm deletes the object |
| Values and Kustomize overlays | no | yes: a `values` resource, read with `${values.path}`, and a `kustomization` resource |
| `dependsOn` | orders the manifests in the file | accepted only where Helm's own install order by kind already agrees; otherwise refused |
| References in a param | another object's name and namespace | those, plus chart values, the release and the chart, each written as a Helm template action |
| Plan | a server-side dry run | a comparison with what Helm last installed |

Examples use `kubernetes`; see [`helm`](../../targets/helm.md) for charts.

## A resource is an object

`type` is API version and kind joined with `/`; `params` are the object's fields
(`metadata`, `spec`, `data`). Custom resources work the same way.

`apiVersion` and `kind` come from the type and cannot be set again in `params`.
An object with no `metadata.name` is named after the resource's `id`, with
underscores turned into hyphens (`web_config` becomes `web-config`).

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web
spec:
  target: kubernetes
  resources:
    - id: web_config
      type: v1/ConfigMap
      params:
        data:
          nginx.conf: "server { listen 80; }\n"
      notifies:
        - resource: web
          action: restart

    - id: web
      type: apps/v1/Deployment
      params:
        spec:
          replicas: 2
          selector:
            matchLabels: {app: web}
          template:
            metadata:
              labels: {app: web}
            spec:
              containers:
                - name: nginx
                  image: nginx:1.27
              volumes:
                - name: config
                  configMap:
                    name: ${web_config}

    - id: web_svc
      type: v1/Service
      params:
        spec:
          type: LoadBalancer
          selector: {app: web}
          ports: [{port: 80}]

  outputs:
    - name: web_url
      value: http://${web_svc.status.loadBalancer.ingress[0].ip}
```

[`examples/flavours/container-orchestration.yaml`](../../../examples/flavours/container-orchestration.yaml).
Longer:
[`examples/kubernetes/web.yaml`](../../../examples/kubernetes/web.yaml).

## The settings resource

An optional `cluster` resource sets the kubeconfig `context`, default `namespace` and
`kubeconfig` file.

## `state`

Left out. Applying never deletes; `state: absent` is refused.

## References

In `params`, a reference can only read what is known at compile:

| You write | You get |
|---|---|
| `${web_config}` or `${web_config.metadata.name}` | the object's name |
| `${web_config.metadata.namespace}` | its namespace |

Anything else is refused. An **output** can read any path in the live object.

## Order

`dependsOn` orders the manifests only; nothing waits for readiness. The compile warns.

## `notifies`

Synthetic: `restart` on a Deployment, StatefulSet or DaemonSet. Meridian writes a checksum of
the notifier into the pod template annotation `meridian.dev/restart-on-<name>`, so a change
rolls the pods. The compile warns. Anything else is refused.

## `runtimeWhen`

Refused.

## `adopt`

| Target | `adopt` is |
|---|---|
| `kubernetes` | accepted with a warning, and changes nothing: `kubectl apply` takes over an object of the same kind, name and namespace |
| `helm` | refused: Helm refuses an object that no release of its own owns |

## Outputs

Meridian waits for an output that has no value yet, such as a load balancer address
(timeout on the target page). One that never arrives is an error. Values from a Secret are
sensitive.

## Plan and apply

`meridian plan` is a server-side dry run. Objects in a namespace the document creates
cannot be previewed until it exists; that plan is unknown.

## How this differs from containerization

[Containerization](containerization.md) brings one project up on one host while Meridian
waits. Here the cluster converges submitted objects afterwards.
