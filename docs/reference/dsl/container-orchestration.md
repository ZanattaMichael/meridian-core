# The container orchestration flavour

Container orchestration tools hand objects to a cluster, and the cluster's
controllers make them true. This page is how the [Meridian language](index.md)
is written for them.

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

Both targets take the same objects, spelt the same way, so a document moves from
one to the other by changing `spec.target` and its settings resources.

| | `kubernetes` | `helm` |
|---|---|---|
| Compiles to | plain manifests | a Helm chart |
| Installed with | `kubectl apply` | `helm upgrade --install`, as one release with history and rollback |
| Removing a resource from the document | the object stays until you delete it | Helm deletes the object |
| Values and Kustomize overlays | no | yes: a `values` resource, read with `${values.path}`, and a `kustomization` resource |
| `dependsOn` | orders the manifests in the file | accepted only where Helm's own install order by kind already agrees; otherwise refused |
| References in a param | another object's name and namespace | those, plus chart values, the release and the chart, each written as a Helm template action |
| Plan | a server-side dry run | a comparison with what Helm last installed |

The rest of this page uses the `kubernetes` target for its examples.
[The `helm` page](../../targets/helm.md) has the same sections for charts.

## A resource is an object

A resource's `type` is the object's API version and kind, joined with `/`, and
its `params` are the object's fields exactly as the API documents them:
`metadata`, `spec`, `data`. Custom resources work the same way, because the type
is not looked up anywhere: the cluster decides whether it exists.

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

This document is
[`examples/flavours/container-orchestration.yaml`](../../../examples/flavours/container-orchestration.yaml).
A longer one:
[`examples/kubernetes/web.yaml`](../../../examples/kubernetes/web.yaml).

## The settings resource

One optional resource of type `cluster` says where the objects go: the
kubeconfig `context`, a default `namespace`, another `kubeconfig` file. It is
not a manifest.

## `state`

Left out. Applying manifests never deletes an object, so `state: absent` is
refused: delete the object with the cluster's own tool and remove it from the
document.

## References

A cluster does not wire one object's live state into another's spec, so inside
`params` a reference can only be something known when the document is compiled:

| You write | You get |
|---|---|
| `${web_config}` or `${web_config.metadata.name}` | the object's name |
| `${web_config.metadata.namespace}` | its namespace |

Anything else in a param is refused with the reason. In an **output** a
reference can be any path into the live object, because an output is read from
the cluster after the apply.

## Order

`dependsOn` orders the manifests in the file and nothing more. The objects are
submitted in that order without waiting for one to be ready, and the compile
warns about it. Readiness is the cluster's business: write the document so that
the order of submission does not matter, as the cluster's own tooling expects.

## `notifies`

Synthetic, with one action: `restart`, aimed at a Deployment, StatefulSet or
DaemonSet. A cluster does not restart pods because a ConfigMap or Secret they
use changed. Meridian writes a checksum of the notifier into an annotation on
the workload's pod template, `meridian.dev/restart-on-<name>`. When the content
changes, the template changes and the workload's controller rolls its pods.
The compile warns about this. Any other action, or any other kind
of notified object, is refused.

## `runtimeWhen`

Refused.

## Outputs

An output can take time to exist: a load balancer's address appears some while
after the Service is accepted. After an apply Meridian waits for an output that
has no value yet (how long is on the target's page). One that never arrives is
an error, not an empty value. Anything read from a Secret is reported as
sensitive.

## Plan and apply

`meridian plan` is a server-side dry run, so the cluster itself says what would
change. Nothing is stored in a dry run, so objects in a namespace the same
document creates cannot be previewed until the namespace exists; that plan is
reported as unknown.

## How this differs from containerization

A [containerization](containerization.md) document is one project on one host,
brought up in order while Meridian waits. Here Meridian submits objects and the
cluster converges them afterwards, which is why order means less and outputs may
have to be waited for.
