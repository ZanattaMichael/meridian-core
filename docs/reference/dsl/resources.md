# Resources: `id`, `type`, `state`, `params`

A resource is one thing a document wants to be true: a package installed, a
machine existing, an object in a cluster. `spec.resources` is a list of them,
and it must hold at least one.

```yaml
- id: nginx_conf
  type: file
  state: present
  params:
    path: /etc/nginx/nginx.conf
    content: "worker_processes 4;\n"
```

This page covers the four fields that say what the resource is. The fields that
tie it to other resources have their own pages: [`dependsOn`](dependson.md),
[`notifies`](notifies.md), [`when` and `runtimeWhen`](conditions.md).

## `id`

The resource's name inside its document. Required.

It is used in four places, which is why it has to be unique in the document:

- `dependsOn` and `notifies` name a resource by its id
- `${id}` and `${id.path}` refer to it
- plans, warnings and errors report it
- most targets use it as a name in the tool's own file: the Terraform block
  name, the Bicep symbolic name, the Compose service name, the title of a
  Puppet resource, the name of an Ansible task

```
dup.yaml:11:7: spec.resources[1].id: duplicate resource id "a" (first declared at dup.yaml:8:7)
```

**What it may hold.** The core accepts any text. The tools are stricter,
because the id ends up in their files. Letters, digits and underscores, starting
with a letter, work on every target. Where a tool needs another spelling, the
target derives it and keeps reporting the id you wrote:

| Target | What the id becomes |
|---|---|
| `cloudformation` | a logical id of letters and digits: `web_vm` is written `WebVm` |
| `kubernetes` | the object's name when `metadata.name` is not given, with underscores as hyphens: `web_config` is `web-config` |
| `terraform`, `opentofu`, `pulumi`, `bicep`, `arm`, `docker` | used as written |

An id is local to its document. Two documents may both have a resource called
`web`; nothing connects them.

## `type`

What kind of thing the resource is. Required. Its meaning belongs to the
target's segment:

| Segment | `type` is | Reference |
|---|---|---|
| Configuration management, shared dialect (`ansible`, `puppet`, `openvox`, `chef`, `salt`, `cloud-init`) | one of six words: `package`, `file`, `service`, `user`, `group`, `exec`; or the tool's own type beside them, such as `ansible.builtin.lineinfile` | [flavour](configuration-management.md), [types](../resource-types.md), [native types](configuration-management.md#native-types-beside-the-six) |
| Configuration management, DSC | `Module/Resource` | [flavour](configuration-management.md#the-native-dialect-dsc) |
| Infrastructure as code | the tool's own type: `aws_instance`, `AWS::EC2::Instance`, `Microsoft.Network/publicIPAddresses@2023-09-01` | [flavour](infrastructure-as-code.md) |
| Containerization | an element of the project: `service`, `network`, `volume`, `config`, `secret` | [flavour](containerization.md) |
| Container orchestration | API version and kind: `apps/v1/Deployment` | [flavour](container-orchestration.md) |

The same word can mean different things in different segments. `service` is an
operating system service to Ansible and a container to Docker. The target
decides, and nothing is shared between them.

A type the target does not have is refused, and the message lists the ones it
has:

```
type.yaml:8:7: ansible: resource "a" [unsupported-resource-type]: unsupported resource type "cron" on this target (supported: exec, file, group, package, service, user)
```

Targets that pass a tool's own types through (most of them) cannot know every
type the tool has, so they check the type's *shape* and leave its existence to
the tool: a Bicep type without its API version is refused at compile; a
Terraform type that no provider has is refused by Terraform when it plans.

**Settings resources.** Most passthrough targets reserve one or two type names
for a resource that is not a thing to create but says where and how the rest is
deployed: `provider` and `backend` (Terraform), `stack` (CloudFormation), `provider`, `backend` and `encryption` (OpenTofu),
`stack` and `config` (Pulumi), `deployment` and `parameter` (ARM), `variable`,
`local` and `plugin` (Packer),
`deployment` (Bicep), `project` (Docker), `cluster` (Kubernetes), `values`,
`chart`, `release` and `kustomization` (Helm). It is written in the same list so that a document stays one list. Each
target's page gives its params and whether it is required. The `dsc` target has
none any more: how a DSC configuration is run is said under
[`spec.settings`](index.md#settings), and its old `runner` resource compiles with
a warning. Other targets' settings resources will move the same way.

## `state`

What "as it should be" means for this resource. Optional text.

Only the shared configuration management types use it, because only they
describe a thing that can be in several conditions on a machine:

| Type | States |
|---|---|
| `package` | `present` (default), `absent`, `latest` |
| `service` | `running` (default), `started`, `stopped` |
| `file` | `present` (default) or `file`, `directory`, `absent` |
| `user`, `group` | `present` (default), `absent` |
| `exec` | none |

Left out, the type's default applies. A state the type does not have on the
chosen target is refused:

```
st.yaml:8:7: ansible: resource "a" [unsupported-state]: resource type "package" does not accept state "purged" on this target (accepted: absent, latest, present)
```

Every other flavour refuses `state`, each for its tool's own reason, and the
message says what to do instead:

| Target | Why there is no `state` |
|---|---|
| `dsc` | a DSC resource carries its desired state in its own properties (`Ensure`, `_exist`); set it in `params` |
| `terraform`, `opentofu`, `pulumi`, `cloudformation`, `docker`, `podman`, `helm` | the tool removes what the document no longer declares; remove the resource |
| `bicep`, `arm`, `kubernetes` | the tool never deletes; remove the resource from the document and delete it with the tool |
| `packer` | a build only creates images; remove the resource |

## `params`

The type's settings. An optional mapping, and the one place in a document where
the core does not check field names: `params` is free-form to the language and
belongs entirely to the target.

**Values keep their type.** A param can be text, a number, a boolean, null, a
list or a mapping, nested as deep as the tool's own format goes. The value
reaches the tool as the type you wrote, so [how YAML types a plain
value](index.md#files) matters here:

```yaml
params:
  mode: "0644"       # text. Unquoted, 0644 would be the number 420
  enabled: true      # a boolean
  enabled: yes       # text, not a boolean
  replicas: 2        # a number
```

**Who checks what.**

| Flavour | How strictly params are checked at compile |
|---|---|
| The six shared types | completely. Every param is known; one the type does not take on that target is refused, never dropped |
| A tool's own type on a shared-dialect target | lightly, as on a passthrough target: the params are the tool's, and the tool is the judge |
| Passthrough targets | lightly. The names and values are the tool's own, so the tool is the judge. The target checks what it must understand to write the file: references, the settings resource, and the few params it treats specially |

```
param.yaml:8:7: ansible: resource "a" [unsupported-param]: resource type "package" does not accept param "version" on this target (accepted: name)
```

**Spelling follows the tool.** On a passthrough target a param is written the
way the tool documents it: `instance_type` for Terraform, `InstanceType` for
CloudFormation, `spec.replicas` for Kubernetes. Meridian does not translate
names.

**Text values may hold references.** On targets that read them, `${id.path}`
inside a text param is replaced by a value from another resource. See
[references](references.md).

## A resource in each flavour

The same four fields, four meanings:

```yaml
# configuration management: an abstract type, a state, a few known params
- id: nginx_svc
  type: service
  state: running
  params:
    name: nginx
    enabled: true

# infrastructure as code: the tool's type and arguments, no state
- id: web
  type: aws_instance
  params:
    ami: ami-0abcdef1234567890
    instance_type: t3.micro

# containerization: an element of the project, its fields as the format has them
- id: app
  type: service
  params:
    image: ghcr.io/example/app:1.4
    ports: ["8080:8080"]

# container orchestration: an API object, its whole body
- id: web_svc
  type: v1/Service
  params:
    spec:
      selector: {app: web}
      ports: [{port: 80}]
```

Each is part of a complete document under
[`examples/flavours/`](../../../examples/flavours).
