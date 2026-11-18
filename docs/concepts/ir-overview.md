# Documents, resources and targets

Everything you write for Meridian is a **document**: one YAML (or JSON) object
with a kind, a name and a spec.

```yaml
apiVersion: meridian/v1
kind: ResourceSet        # or Infrastructure
metadata:
  name: web
spec:
  target: ansible        # which tool does the work
  resources: [...]
```

## Two kinds

| Kind | Describes | Extra fields |
|---|---|---|
| `Infrastructure` | things to create: machines, networks, containers, cluster objects | `outputs`: values later stages can read |
| `ResourceSet` | how a machine that already exists is configured | `targetHost`: the machine |

They are separate on purpose. See [the two-stage architecture](two-stage-architecture.md).
Each plugin declares a category, and the category decides which kind it takes:

<p align="center"><img src="../graphics/meridian-plugin-categories.svg" alt="Infrastructure documents go to infrastructure as code, containers and orchestration; ResourceSets go to configuration management; docker takes both" width="100%"></p>

Every field and its rules are in
[the language reference](../reference/dsl/index.md), with a page per segment.

## Resources

A resource is one thing the document wants to be true.

```yaml
- id: nginx_conf            # unique in the document; how everything refers to it
  type: file                # what kind of thing
  state: present            # optional; what "true" means for this type
  params:                   # the type's settings
    path: /etc/nginx/nginx.conf
    content: "worker_processes 4;\n"
  dependsOn:                # optional; see Dependencies
    - resource: nginx_pkg
  notifies:
    - resource: nginx_svc
      action: restart
  runtimeWhen: "..."        # optional; only on targets that can check at apply time
```

## Two families of types

What `type` and `params` mean depends on the target's category.

**Configuration management targets** (`ansible`, `puppet`, `openvox`, `chef`, `salt`,
`cloud-init`) share six abstract types: `package`, `file`, `service`, `user`,
`group` and `exec`. The same resource compiles for any of them, and each target
page shows what a type and its params become. This is where "write once" is
real, because these tools manage the same small set of things. `cloud-init`
applies the six once, on first boot, which narrows some of them;
[its page](../targets/cloud-init.md) explains. Where the six fall short, these
targets also take the tool's own types as the tool writes them
(`ansible.builtin.lineinfile`), with the tool's params; such a resource compiles
for that one target
([native types beside the six](../reference/dsl/configuration-management.md#native-types-beside-the-six)).

**Everything else is native passthrough.** A Terraform provider has thousands of
resource types, Kubernetes has its whole API, Azure has every service. Meridian
does not hide them behind a smaller, weaker vocabulary. `type` is the tool's own
type and `params` are its arguments, exactly as the tool documents them:

| Target | `type` is | Example |
|---|---|---|
| `terraform` | the Terraform type | `aws_instance` |
| `opentofu` | the OpenTofu type | `aws_instance` |
| `cloudformation` | the CloudFormation type | `AWS::EC2::Instance` |
| `bicep` | the Azure type and API version | `Microsoft.Network/publicIPAddresses@2023-09-01` |
| `arm` | the Azure type; the API version is the `apiVersion` param | `Microsoft.Network/publicIPAddresses` |
| `packer` | the part of a build, then Packer's own type | `source.amazon-ebs`, `provisioner.shell` |
| `pulumi` | the Pulumi type token | `aws:ec2:Instance` |
| `kubernetes` | API version and kind | `apps/v1/Deployment` |
| `helm` | API version and kind, the same as `kubernetes` | `apps/v1/Deployment` |
| `docker` | an element of a Compose file | `service` |
| `podman` | a kind of Quadlet unit | `container` |
| `dsc` | module and resource | `PSDscResources/Service` |

For these targets Meridian adds everything around the resource: one document
format, compile-time-checked references, dependencies, notifications where the
tool can react, outputs for the next stage, and one way to plan and apply.

Most passthrough targets also take one **settings resource** that is not part of
the tool's file: where to deploy, which cluster, which daemon. It has a fixed
type name (`stack`, `deployment`, `cluster`, `project`) and is
described on the target's page. The `dsc` target takes its settings under
`spec.settings` instead, one mapping for the whole document, which is where the
others will move. The `helm` target has four: `values`, `chart`,
`release` and `kustomization`. The `arm` target has one more type of its own,
`parameter`, which declares a parameter of the template. `packer` has none: one document is one build,
and everything a build needs is a resource of it. The `terraform` and `opentofu`
targets have no settings resource of that kind either; their `provider` and
`backend` resources, and `opentofu`'s `encryption`, `variable` and `local`,
become blocks of the configuration itself.

## References

`${id}` or `${id.path}` in a param refers to another resource in the same
document. Meridian checks it exists, and each target rewrites it into the tool's
own form: a Terraform or OpenTofu address, a CloudFormation `Ref`,
a Packer variable, the name of a Kubernetes object, a `{{ .Values }}` expression in a Helm chart. What a path can read depends on the tool; see
the target's page. Write `$${` for a literal `${`.

The configuration management targets have nothing to read at apply time. In a
ResourceSet for `ansible`, `puppet`, `openvox`, `chef`, `salt` or `cloud-init`,
`${id.param}` becomes that param's value at compile time, and anything else in
`${...}` is refused
([the rules](../reference/resource-types.md#references)).

## What is not here yet

The design includes hierarchical data (values merged from layers, per
environment or node) and facts gathered from the node. The hierarchy engine
exists, but neither is wired into the `meridian` command yet.

Today a compile-time [`when`](../reference/dsl/index.md#when-and-runtimewhen) is
decided from `--data` files, so it can use anything in a file but nothing about
the node (`facts.*` is refused). `runtimeWhen` works on targets that support it.
