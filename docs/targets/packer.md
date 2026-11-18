# Target: `packer`

<img src="../branding/logos/meridian-packer.svg" width="72" alt="Meridian Packer plugin logo">

Compiles an Infrastructure document into a [Packer](https://developer.hashicorp.com/packer)
build template and runs it with `packer`. Use it to build machine images and container
images: an AMI, an Azure or Google Cloud image, a VMware template, a Docker image.

## What it does and does not support

| | |
|---|---|
| Resource types | Packer's own, after a prefix that says what part of a build the resource is: `source.<builder>`, `provisioner.<type>`, `post-processor.<type>`, `error-cleanup-provisioner.<type>`, `data.<type>`, plus `variable`, `local` and `plugin` |
| Document kind | `Infrastructure`. One document is one Packer build |
| `dependsOn` | Yes, inside the fixed shape of a build: it orders provisioners and chains post-processors into a sequence. A dependency that contradicts the shape is refused. See [Order](#order) |
| `notifies` | No. Refused: a build runs each step once on a machine it then discards. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused. Use a step's own `only` or `except` param |
| `state: absent` | No. A build only creates images; remove the resource from the document |
| References | Yes, to a `variable`, a `local` or a `data.<type>`, checked at compile time. See [References](#references) |
| Plan | Yes, but Packer has no plan of its own: Packer validates the template, and the plan lists one image to create per source. A plan is never "in sync" |
| Apply | Yes: `packer build`. Every apply builds a new image |
| Outputs | Yes, read from the manifest every build ends by writing. See [Outputs](#outputs) |
| Sensitive values | A `variable` can be `sensitive`. Outputs are image ids and are never sensitive |
| Category | `infrastructure-as-code`. See [Why this category](#why-this-category) |
| Compiles to | `build.pkr.json`, plus `meridian-build.json` for Meridian's own use |
| Needs | `packer` 1.7 or later on the machine running Meridian |
| Binary | `meridian-target-packer` |
| Tested against | The real `packer` 1.11 in CI, with the null builder: validate, build, outputs, a rejected template and a failed build. The cloud builders have not been run against a real account |

## Contents

- [What it does and does not support](#what-it-does-and-does-not-support)
- [A first build](#a-first-build)
- [How a document maps to a build](#how-a-document-maps-to-a-build)
- [Resource types](#resource-types)
- [References](#references)
- [Resource types in detail](#resource-types-in-detail)
- [Order](#order)
- [Notifications](#notifications)
- [Outputs](#outputs)
- [Running a build](#running-a-build)
- [Secrets](#secrets)
- [What the plugin refuses](#what-the-plugin-refuses)
- [Examples](#examples)
- [Why this category](#why-this-category)
- [Testing](#testing)

## A first build

This document needs no cloud account and no Packer plugin. The null builder builds
nothing, and every step runs on the machine running Packer.

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: hello
spec:
  target: packer
  resources:
    - id: greeting
      type: variable
      params:
        type: string
        default: hello

    - id: box
      type: source.null
      params:
        communicator: none

    - id: say
      type: provisioner.shell-local
      params:
        inline:
          - echo "${greeting} from ${source.name}"

  outputs:
    - name: builder
      value: ${box.builder_type}
```

```console
$ meridian plan hello.yaml
hello (Infrastructure, target packer)
  + create box: null image

$ meridian apply hello.yaml
  hello | ==> hello.null.box: Running local shell script: ...
  hello |     hello.null.box: hello from box
hello (Infrastructure, target packer)
  + create box: null image
  applied 1 change
  output builder = "null"
```

`meridian compile hello.yaml --out build/` writes the template without running anything,
so you can read exactly what Packer is given.

## How a document maps to a build

**One document is one Packer build.** The build is named after the document
(`metadata.name`) and builds every source the document declares. For a second build, write
a second document.

The plugin does not hide Packer's vocabulary behind a smaller one. After the prefix
that says what part of a build a resource is, its `type` is Packer's type and its
`params` are that type's arguments, as Packer's documentation lists them.

What Meridian adds:

- **Order from the graph.** Provisioners run in the order `dependsOn` gives them, and
  post-processors that depend on one another become a sequence.
- **References by id.** `${region}` refers to the variable with the id `region`. The
  plugin writes Packer's own address for it and refuses a reference to something the
  document does not declare.
- **Outputs.** Packer has none. The plugin records what each source produced and reports
  the values the document asks for, so a later stage can use the id of the image.
- **A check before anything runs.** A document that Packer could not run the way it is
  written is refused at compile time, with the line of the document that is wrong.

The template is written in Packer's JSON syntax, not HCL. In HCL a nested block and a
map-valued argument are written differently, and only a Packer plugin's schema says which
one a param is. In the JSON syntax Packer decides that itself, so a document compiles
correctly without Meridian knowing any plugin's schema.

## Resource types

| `type` | Becomes | Notes |
| --- | --- | --- |
| `source.<builder>` | a `source` block, built by the build | `source.amazon-ebs`, `source.docker`, `source.azure-arm`, `source.null` and any other builder. At least one is required. |
| `provisioner.<type>` | a `provisioner` of the build | `provisioner.shell`, `provisioner.file`, `provisioner.ansible` and so on. |
| `post-processor.<type>` | a `post-processor` of the build | `post-processor.docker-tag`, `post-processor.compress` and so on. |
| `error-cleanup-provisioner.<type>` | the build's `error-cleanup-provisioner` | At most one. It runs only when the build fails. |
| `data.<type>` | a `data` block | `data.amazon-ami`, `data.http` and so on. |
| `variable` | a `variable` block | Params are the block's arguments: `type`, `default`, `description`, `sensitive`. |
| `local` | an entry of `locals` | Takes one param, `value`. |
| `plugin` | an entry of `required_plugins` | Takes `source` and `version`. The id is the plugin's name. `packer init` installs it. |

A resource's id is the block's name, so it must start with a letter or underscore and hold
only letters, digits, underscores and hyphens.

Arguments Packer accepts on every provisioner and post-processor, such as `only`, `except`,
`override`, `pause_before`, `max_retries`, `timeout` and `keep_input_artifact`, are params
like any other. In `only` and `except` a source can be named by its id alone: the plugin
writes `amazon-ebs.ubuntu` for `ubuntu`.

`state` must be `present` or left out, since a build only creates images. `notifies` and
`runtimeWhen` are refused: see [Notifications](#notifications) and
[What the plugin refuses](#what-the-plugin-refuses). Each type's params are in
[Resource types in detail](#resource-types-in-detail).

## References

`${...}` in a param is a reference or an expression.

| You write | Packer is given | When |
| --- | --- | --- |
| `${region}` | `${var.region}` | `region` is a `variable` of the document |
| `${stamp}` | `${local.stamp}` | `stamp` is a `local` |
| `${base.id}` | `${data.amazon-ami.base.id}` | `base` is a `data.amazon-ami` |
| `${source.name}`, `${build.ID}`, `${path.root}`, `${packer.version}` | unchanged | Packer's own names: `var`, `local`, `data`, `source`, `build`, `path`, `packer` |
| `${formatdate("YYYYMMDD", timestamp())}` | unchanged | anything that is not a plain path is Packer's expression language |
| `$${HOME}` | `$${HOME}` (a literal `${HOME}`) | the escape for a literal `${`, in every Meridian document |

Two things follow from this:

- A plain path that names nothing, such as `${regoin}`, is refused at compile time. Without
  the check, the mistake would surface as a Packer error about a file you did not write.
- A shell script that uses `${HOME}` must write `$${HOME}`, because `${` starts a
  reference. `$HOME` and `$(date)` need no escape. Packer's older `{{ .Vars }}` templates
  are plain text to Meridian and pass through untouched.

A source, a provisioner, a post-processor and a plugin requirement have nothing to read
inside a param. Inside a provisioner, Packer's own `${source.name}` and `${build.ID}`
describe the source being built.

## Resource types in detail

Every resource takes the fields every Meridian resource takes: `id`, `type`, `params`,
`dependsOn` and `state` (which must be `present` or left out). What
`params` holds depends on the type.

### `source.<builder>`

What to build from, and where. `<builder>` is any Packer builder: `amazon-ebs`,
`azure-arm`, `googlecompute`, `docker`, `vsphere-iso`, `qemu`, `null`, `file` and so on.

| | |
|---|---|
| Params | The builder's own arguments, unchanged. Nested blocks such as `launch_block_device_mappings` or `source_image_reference` are written as maps or lists of maps |
| Required | At least one source per document. What each builder requires is the builder's business, and `packer validate` checks it during a plan |
| Id | The source's name. Packer shows the build as `<document>.<builder>.<id>` |
| Readable | Nothing inside a param. In outputs: `id`, `artifact_id`, `builder_type`, `build_time`, `files`, `packer_run_uuid` |
| Dependencies | A source cannot depend on another source: Packer builds them at the same time |

A document with several sources builds them all in parallel, and every provisioner and
post-processor runs for each one unless its `only` or `except` says otherwise.

### `provisioner.<type>`

A step run on the machine being built, or on the machine running Packer for `shell-local`.
`<type>` is any Packer provisioner: `shell`, `file`, `powershell`, `ansible`,
`ansible-local`, `breakpoint`, `shell-local`, `windows-restart` and so on.

| | |
|---|---|
| Params | The provisioner's own arguments, plus the ones Packer accepts on every provisioner: `only`, `except`, `override`, `pause_before`, `max_retries`, `timeout` |
| `only`, `except` | A list of sources. A source may be named by its id alone (`ubuntu`), and is written as Packer wants it (`amazon-ebs.ubuntu`). Anything else is left as written |
| Order | Dependency order, then the order of the document |
| Readable | Nothing. Inside its params, Packer's own `${source.name}`, `${source.type}` and `${build.*}` describe the source being built |

### `post-processor.<type>`

A step run on what was built. `<type>` is any Packer post-processor: `docker-tag`,
`docker-push`, `compress`, `checksum`, `manifest`, `shell-local`, `vagrant` and so on.

| | |
|---|---|
| Params | The post-processor's own arguments, plus `only`, `except`, `keep_input_artifact` and `name` |
| Input | With no dependency on another post-processor, the artifact the build produced. With one, the artifact that post-processor produced |
| Limits | A post-processor takes one input, so it can depend on at most one other post-processor, and at most one can depend on it. To use an artifact twice, declare the first post-processor again under another id |

### `error-cleanup-provisioner.<type>`

A provisioner that runs only when the build has failed, before Packer tears the machine
down. Its params are a provisioner's. A document has at most one, and nothing can depend
on it.

### `data.<type>`

A lookup done before the build starts. `<type>` is any Packer data source: `amazon-ami`,
`amazon-secretsmanager`, `http`, `null` and so on. Params are the data source's own
arguments. `${id.attribute}` reads what it found.

### `variable`

An input to the build. The id is the variable's name.

| Param | Meaning |
|---|---|
| `type` | Packer's type, as text: `string`, `number`, `bool`, `list(string)`, `map(string)` |
| `default` | The value when none is given. A variable without one must be set, or Packer refuses to validate |
| `description` | Shown by Packer when it asks about the variable |
| `sensitive` | `true` keeps the value out of Packer's output |

Any other argument of Packer's `variable` block is passed through as written.

`${id}` reads it. It is set by an input of the same name from an earlier stage, or by
`PKR_VAR_<id>` in the environment.

### `local`

A named value computed once. It takes exactly one param, `value`, which can be text, a
number, a list or a map, and can hold references and Packer expressions. `${id}` reads it.

### `plugin`

A Packer plugin the build needs. The id is the plugin's name.

| Param | Meaning |
|---|---|
| `source` | Where the plugin comes from, such as `github.com/hashicorp/amazon`. Required |
| `version` | A version constraint, such as `>= 1.3.0`. Required |

`packer init` installs it the first time the document is planned or applied. The builders
`null` and `file`, and the provisioners and post-processors that ship inside Packer, need
no plugin.

## Order

A Packer build has a fixed shape: install plugins, read variables, look up data, start
every source, run the provisioners, post-process the result. `dependsOn` works inside that
shape.

**Provisioners** run in dependency order. Where no dependency decides, they run in the
order they are written.

```yaml
- id: configure
  type: provisioner.shell
  params: {inline: ["sudo systemctl enable nginx"]}
  dependsOn:
    - resource: install      # declared below, runs first

- id: install
  type: provisioner.shell
  params: {inline: ["sudo apt-get install -y nginx"]}
```

**Post-processors** that depend on one another form a sequence, in which each takes the
artifact of the one before it. A post-processor with no such dependency starts a sequence
of its own and works on what the build itself produced.

```yaml
- id: tag
  type: post-processor.docker-tag
  params: {repository: registry.example.com/team/app, tags: ["1.0.0"]}

- id: push
  type: post-processor.docker-push
  params: {login: false}
  dependsOn:
    - resource: tag          # push takes the tagged image

- id: checksum               # a sequence of its own
  type: post-processor.checksum
  params: {checksum_types: ["sha256"]}
```

compiles to

```json
"post-processors": [
  { "post-processor": [ { "docker-tag": { "...": "..." } }, { "docker-push": { "login": false } } ] },
  { "post-processor": [ { "checksum": { "checksum_types": ["sha256"] } } ] },
  { "post-processor": [ { "manifest": { "output": "meridian-manifest.json", "strip_path": true } } ] }
]
```

The last sequence is the plugin's own: see [Outputs](#outputs).

A dependency that only restates the shape, such as a provisioner depending on a source, is
accepted and changes nothing. One that contradicts it is refused, because writing the
template anyway would run the steps in an order the document ruled out.

## Notifications

`notifies` is refused on this target, whatever the action. A notify means "when this
really changes, do that to the other", and a build has no such moment: every provisioner
runs exactly once, in order, on a machine that did not exist before the build and is
discarded after it. There is nothing to compare with, so nothing can be said to have
changed.

What a notify would be used for elsewhere is simply the next step here. To restart a
service after writing its configuration, add a provisioner that depends on the one that
writes the file:

```yaml
- id: nginx_conf
  type: provisioner.file
  params: {source: /srv/build/nginx.conf, destination: /tmp/nginx.conf}

- id: restart_nginx
  type: provisioner.shell
  params: {inline: ["sudo mv /tmp/nginx.conf /etc/nginx/nginx.conf", "sudo systemctl restart nginx"]}
  dependsOn:
    - resource: nginx_conf
```

## Outputs

Every build the plugin compiles ends with Packer's built-in `manifest` post-processor,
which records what each source produced in `meridian-manifest.json` in the work directory.
A document's outputs read from it, as `${<source id>.<attribute>}`:

| Attribute | Value |
| --- | --- |
| `id` | The image's id. For a single-region image from an Amazon builder the region is removed: `ami-0abc1234`. Otherwise it is the artifact id unchanged. |
| `artifact_id` | The artifact's id exactly as Packer reports it, such as `us-east-1:ami-0abc1234`. |
| `builder_type` | The builder that made it, such as `amazon-ebs`. |
| `build_time` | When the build finished, in seconds since 1970. |
| `files` | The files of the artifact, each with its name and size, for a builder that produces files. |
| `packer_run_uuid` | The id of the `packer` run that made it. |

```yaml
outputs:
  - name: image_id
    value: ${ubuntu.id}
  - name: image_label
    value: web built from ${ubuntu.artifact_id}
```

An output that is nothing but one reference keeps the type of what it reads, so
`${ubuntu.build_time}` is a number. Anything else is text.

Outputs always describe the last build. The manifest keeps earlier runs too, and the
plugin ignores them. `meridian` reads the outputs back without building again whenever a
later stage needs them.

An output can only read what a build produced, so a reference to a variable, a local or a
data source in an output is refused.

## Running a build

| Command | What the plugin runs | What changes |
| --- | --- | --- |
| `meridian compile` | nothing | nothing |
| `meridian plan` | `packer init .`, `packer validate .` | nothing is built; plugins the template requires are installed |
| `meridian apply` | `packer init .`, `packer validate .`, `packer build -color=false .` | every source is built |

Packer has no plan of its own. It does not need one to be honest: a build never changes an
existing image, it always makes a new one. So a plan lists one creation per source, after
Packer itself has checked the template. A plan is therefore never "in sync", and applying
twice builds two images.

Packer runs in the document's work directory, which Meridian keeps under its state
directory (`.meridian/` in the directory `meridian` is run from, or `--state-dir`). A
relative path in a param, such as the `source` of a `provisioner.file`, is relative to
that work directory and not to the document, so use an absolute path.

**Variables.** Values handed over from an earlier stage reach Packer as `PKR_VAR_<name>`,
so a `variable` with the same id receives them. Packer ignores a value it has no variable
for. Variables can also be set the usual way, with `PKR_VAR_<name>` in the environment
Meridian runs in.

**Credentials.** The plugin passes its environment through to Packer. Sign in the way the
builder expects, for example `AWS_PROFILE` or `ARM_CLIENT_ID`. A value a step needs that
must not be written into the template is a [secret](#secrets).

**A different `packer`.** Set `MERIDIAN_PACKER` to the program to run when it is not
`packer` on `PATH`. `MERIDIAN_PACKER_ARGS` adds arguments to `packer validate` and
`packer build`, split like a shell command line (quotes hold a word together): `-only=amazon-ebs.web`, `-parallel-builds=1`.
Both can also be written in the project's `meridian.yaml` under `spec.settings.packer`
as `program` and `args`; see the [plugin settings reference](../reference/plugin-settings.md#packer).

When Packer fails, Meridian reports which step failed, the command that was run, the exit
status and the end of what Packer printed.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) in a resource's params
reaches Packer as an input variable read from a private file, and through nothing else.

```yaml
spec:
  target: packer
  secrets:
    - name: token
      from: env
      key: REGISTRY_TOKEN
  resources:
    - id: token
      type: local
      params:
        value: ${{ secret.token }}
    - id: login
      type: provisioner.shell-local
      params:
        inline:
          - printf '%s' "${token}" | docker login --password-stdin -u ci registry.example.com
```

**What the compile writes.** The param becomes `${var.meridian_secret_<name>}`, and the
template declares that variable with `type = string` and `sensitive = true`, so Packer
prints `<sensitive>` wherever the value would appear in its output. The compiled template
holds the variable's name and never its value, so it can be reviewed and kept.

**How the value reaches the tool.** When a stage runs, the runner writes every secret into
one variables file, `secrets.pkrvars.json`, in a directory only the user can enter (mode
`0700`, the file `0600`, on a memory filesystem where the system has one), and passes it as
`-var-file=<path>` to `packer validate` and `packer build`. `packer init` has no use for
it and is not given it. The file is overwritten and removed when the build ends. The
command line names only the file's path, and Packer's environment holds nothing of the
value. Packer runs a step that uses the value like any other step: a `shell-local`
step's commands go into a script in Packer's temporary directory for the length of the
step.

**What is refused.** A secret in the declaration of a `variable` or a `plugin` is refused
at compile, rule `unsupported-secret-place`: a declaration is written into the template
as it stands. Put the secret in a `local`, a provisioner, a post-processor or a source,
which are compiled to a reference.

The handover is tested two ways:

- against a stand-in `packer` that records the variables file, its mode, its arguments
  and its environment (`plugins/packer/tests/run.rs`)
- against the real `packer` in CI (`plugins/packer/tests/packer.rs`): a step writes the
  value to a file, and the value is not in Packer's output or any other file the plugin
  wrote

## What the plugin refuses

Meridian never compiles a document into something that quietly does less than it says.
Each refusal names the resource, the rule and the line of the document.

| Rule | When |
| --- | --- |
| `unsupported-resource-type` | The type is not one of the [resource types](#resource-types), such as `package` or `aws_instance`. |
| `missing-param` | The document declares no `source.<builder>`. |
| `invalid-reference` | A `${...}` names something the document does not declare, reads a source or a step inside a param, is never closed, or is an output that does not read a source's attribute. |
| `unsupported-dependency` | A dependency contradicts the shape of a build: a provisioner depending on a post-processor, a source depending on a source, a post-processor depending on two others, two post-processors depending on the same one, a data source or local depending on another without referring to it, or anything depending on the error-cleanup provisioner. |
| `unsupported-action` | The document uses `notifies`. A build runs each step once on a machine it then discards, so nothing in it can react to a change. |
| `unsupported-runtime-condition` | The document uses `runtimeWhen`. To run a step for some sources only, use the step's `only` or `except` param. |
| `unsupported-state` | `state` is anything but `present`. A build never removes an image. |
| `missing-param`, `invalid-param` | A `plugin` without `source` and `version`, a `local` without `value`, or either with a param it does not take. |
| `duplicate-resource` | A second `error-cleanup-provisioner`. |
| `invalid-name` | A resource id that cannot be a Packer block name. |

## Examples

Complete documents are in [`examples/packer/`](../../examples/packer/), and what each
compiles to is in [`plugins/packer/testdata/`](../../plugins/packer/testdata/).

| Example | Shows |
| --- | --- |
| [`hello.yaml`](../../examples/packer/hello.yaml) | A build that runs anywhere: a variable, a local, ordered provisioners, a post-processor sequence, outputs. |
| [`web-image.yaml`](../../examples/packer/web-image.yaml) | An AWS machine image: a required plugin, a data source that finds the base image, four ordered provisioners, the image id as an output. |
| [`app-container.yaml`](../../examples/packer/app-container.yaml) | A container image: tag then push as one sequence, a checksum as another, a sensitive variable, an error-cleanup provisioner. |

### A machine image, start to finish

The heart of [`web-image.yaml`](../../examples/packer/web-image.yaml):

```yaml
- id: base
  type: data.amazon-ami
  params:
    region: ${region}
    most_recent: true
    owners: ["099720109477"]
    filters:
      name: ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*

- id: ubuntu
  type: source.amazon-ebs
  params:
    region: ${region}
    source_ami: ${base.id}
    instance_type: t3.small
    ssh_username: ubuntu
    ami_name: web-${stamp}

- id: install_nginx
  type: provisioner.shell
  params:
    inline:
      - sudo apt-get update
      - sudo apt-get install -y nginx
  dependsOn:
    - resource: wait_for_boot
```

becomes

```json
"data": {
  "amazon-ami": {
    "base": { "region": "${var.region}", "most_recent": true, "...": "..." }
  }
},
"source": {
  "amazon-ebs": {
    "ubuntu": {
      "ami_name": "web-${local.stamp}",
      "region": "${var.region}",
      "source_ami": "${data.amazon-ami.base.id}",
      "...": "..."
    }
  }
}
```

and after `meridian apply`:

```console
  output image_artifact = "us-east-1:ami-0abc1234"
  output image_id = "ami-0abc1234"
```

### One step for some sources only

```yaml
- id: ubuntu
  type: source.amazon-ebs
  params: {region: us-east-1, source_ami: ami-0abc1234, instance_type: t3.small, ssh_username: ubuntu, ami_name: web}

- id: app
  type: source.docker
  params: {image: debian:bookworm-slim, commit: true}

- id: cloud_init
  type: provisioner.shell
  params:
    inline: ["cloud-init status --wait"]
    only: [ubuntu]           # written to Packer as amazon-ebs.ubuntu
```

## Why this category

Every Meridian plugin declares a category, and the plugin protocol has four:
infrastructure as code, configuration management, containerization and container
orchestration. Packer builds images, which is none of them exactly.

It is declared as **infrastructure as code** because that is the closest fit:

- An image is created from nothing by a declarative definition, the way a Terraform
  resource is. Configuration management, by contrast, configures a machine that already
  exists and keeps existing.
- What a build produces is an id that the stage creating machines consumes, so it belongs
  in the same part of a workflow as the tools that provision infrastructure, ahead of them.
- It uses the `Infrastructure` document kind, which is the kind that has outputs.
- Packer builds containers too, but also AMIs and VM templates, so containerization would
  be wrong for most of what it does.

If the protocol gains an image-building category, this plugin should move to it. That is a
one-line change in the plugin.

## Testing

```console
$ cargo test -p meridian-target-packer
```

runs the unit tests, the compile tests with their golden files, the runner tests against a
stand-in for `packer`, and the tests that drive the real plugin binary across the process
boundary. None of them needs Packer.

```console
$ cargo test -p meridian-target-packer --test packer -- --ignored
```

runs the tests that need the real `packer`: its syntax check over every example, and a
whole build, a rejected template and a failing build with the null builder. The `packer`
workflow runs them on every pull request that touches the plugin. After changing what the
plugin emits, record the golden files again with `UPDATE_GOLDEN=1`.
