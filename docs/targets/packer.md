# Target: `packer`

<img src="../branding/logos/meridian-packer.svg" width="72" alt="Meridian Packer plugin logo">

Compiles an Infrastructure document into a [Packer](https://developer.hashicorp.com/packer)
build template and runs it with `packer`, to build machine and container images.

## What it does and does not support

| | |
|---|---|
| Resource types | Packer's own, after a prefix that says what part of a build the resource is: `source.<builder>`, `provisioner.<type>`, `post-processor.<type>`, `error-cleanup-provisioner.<type>`, `data.<type>`, plus `variable`, `local` and `plugin` |
| Document kind | `Infrastructure`. One document is one Packer build |
| `dependsOn` | Yes, inside the fixed shape of a build: it orders provisioners and chains post-processors into a sequence. A dependency that contradicts the shape is refused. See [Order](#order) |
| `notifies` | No. Refused: a build runs each step once on a machine it then discards. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused. Use a step's own `only` or `except` param |
| `adopt` | No. Refused: Packer builds a new image on every run and manages nothing that exists |
| `state: absent` | No. A build only creates images; remove the resource from the document |
| References | Yes, to a `variable`, a `local` or a `data.<type>`, checked at compile time. See [References](#references) |
| Plan | Yes, but Packer has no plan of its own: Packer validates the template, and the plan lists one image to create per source. A plan is never "in sync" |
| Apply | Yes: `packer build`. Every apply builds a new image |
| Outputs | Yes, read from the manifest every build ends by writing. See [Outputs](#outputs) |
| Sensitive values | A `variable` can be `sensitive`. Outputs are image ids and are never sensitive |
| Category | `infrastructure-as-code`: it creates images declaratively and its outputs feed later stages |
| Compiles to | `build.pkr.json`, plus `meridian-build.json` for Meridian's own use |
| Needs | `packer` 1.7 or later on the machine running Meridian |
| Binary | `meridian-target-packer` |
| Tested against | The real `packer` 1.11 in CI, with the null builder: validate, build, outputs, a rejected template and a failed build. The cloud builders have not been run against a real account |

## A first build

No cloud account or plugin needed; the null builder runs every step locally.

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

`meridian compile hello.yaml --out build/` writes the template without running it.

## How a document maps to a build

**One document is one build**, named after `metadata.name`, building every source. After
the prefix, `type` is Packer's type and `params` are its arguments.

Meridian adds:

- **Order** from `dependsOn` for provisioners and post-processor sequences.
- **References by id** (`${region}`), checked at compile.
- **Outputs**, recorded per source.
- **Compile-time checks** against your lines.

The template is Packer JSON, so blocks and maps need no plugin schema.

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

Ids start with a letter or underscore and hold letters, digits, `_` and `-`.

Common arguments (`only`, `except`, `override`, `pause_before`, `max_retries`, `timeout`,
`keep_input_artifact`) are params. In `only` and `except` a source id becomes
`amazon-ebs.ubuntu`.

`state` must be `present` or absent. `notifies` and `runtimeWhen` are
[refused](#what-the-plugin-refuses).

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

- An unknown path such as `${regoin}` is refused.
- Write `$${HOME}` for a shell `${HOME}`. `$HOME`, `$(date)` and `{{ .Vars }}` pass through.

Sources, provisioners, post-processors and plugins cannot be read. In a provisioner,
`${source.name}` and `${build.ID}` are Packer's.

## Resource types in detail

### `source.<builder>`

Any Packer builder: `amazon-ebs`, `azure-arm`, `googlecompute`, `docker`, `qemu`, `null`.

| | |
|---|---|
| Params | The builder's own arguments, unchanged. Nested blocks such as `launch_block_device_mappings` or `source_image_reference` are written as maps or lists of maps |
| Required | At least one source per document. What each builder requires is the builder's business, and `packer validate` checks it during a plan |
| Id | The source's name. Packer shows the build as `<document>.<builder>.<id>` |
| Readable | Nothing inside a param. In outputs: `id`, `artifact_id`, `builder_type`, `build_time`, `files`, `packer_run_uuid` |
| Dependencies | A source cannot depend on another source: Packer builds them at the same time |

Sources build in parallel; steps run for each unless `only` or `except` say otherwise.

### `provisioner.<type>`

Any Packer provisioner: `shell`, `file`, `powershell`, `ansible`, `shell-local`.

| | |
|---|---|
| Params | The provisioner's own arguments, plus the ones Packer accepts on every provisioner: `only`, `except`, `override`, `pause_before`, `max_retries`, `timeout` |
| `only`, `except` | A list of sources. A source may be named by its id alone (`ubuntu`), and is written as Packer wants it (`amazon-ebs.ubuntu`). Anything else is left as written |
| Order | Dependency order, then the order of the document |
| Readable | Nothing. Inside its params, Packer's own `${source.name}`, `${source.type}` and `${build.*}` describe the source being built |

### `post-processor.<type>`

Any Packer post-processor: `docker-tag`, `docker-push`, `compress`, `checksum`.

| | |
|---|---|
| Params | The post-processor's own arguments, plus `only`, `except`, `keep_input_artifact` and `name` |
| Input | With no dependency on another post-processor, the artifact the build produced. With one, the artifact that post-processor produced |
| Limits | A post-processor takes one input, so it can depend on at most one other post-processor, and at most one can depend on it. To use an artifact twice, declare the first post-processor again under another id |

### `error-cleanup-provisioner.<type>`

Runs only on failure. At most one; nothing can depend on it.

### `data.<type>`

Any Packer data source (`amazon-ami`, `http`). `${id.attribute}` reads it.

### `variable`

The id is the variable's name.

| Param | Meaning |
|---|---|
| `type` | Packer's type, as text: `string`, `number`, `bool`, `list(string)`, `map(string)` |
| `default` | The value when none is given. A variable without one must be set, or Packer refuses to validate |
| `description` | Shown by Packer when it asks about the variable |
| `sensitive` | `true` keeps the value out of Packer's output |

Other `variable` arguments pass through. `${id}` reads it; it is set by a same-named input
from an earlier stage or `PKR_VAR_<id>`.

### `local`

One param, `value`, which may hold references and Packer expressions. `${id}` reads it.

### `plugin`

The id is the plugin's name.

| Param | Meaning |
|---|---|
| `source` | Where the plugin comes from, such as `github.com/hashicorp/amazon`. Required |
| `version` | A version constraint, such as `>= 1.3.0`. Required |

`packer init` installs it on first plan or apply. Built-in types need none.

## Order

A build's shape is fixed: plugins, variables, data, sources, provisioners,
post-processors.

**Provisioners** run in dependency order, then written order.

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

**Post-processors** that depend on each other form a sequence; others start their own.

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

The last sequence is the plugin's ([Outputs](#outputs)). A dependency restating the shape
changes nothing; one contradicting it is refused.

## Notifications

Refused: every provisioner runs once on a fresh machine. Use a dependent provisioner
instead:

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

Every build ends with a `manifest` post-processor writing `meridian-manifest.json`. Outputs
read `${<source id>.<attribute>}`:

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

A single reference keeps its type; anything else is text. Outputs describe the last build
and are read back without rebuilding. Variables, locals and data sources cannot be outputs.

## Running a build

| Command | What the plugin runs | What changes |
| --- | --- | --- |
| `meridian compile` | nothing | nothing |
| `meridian plan` | `packer init .`, `packer validate .` | nothing is built; plugins the template requires are installed |
| `meridian apply` | `packer init .`, `packer validate .`, `packer build -color=false .` | every source is built |

A build always makes new images, so a plan lists one creation per source after
`packer validate`. It is never "in sync"; applying twice builds twice.

Packer runs in the work directory under `--state-dir`, so use absolute paths in params.

**Variables** from earlier stages arrive as `PKR_VAR_<name>`.

**Credentials** come from the environment (`AWS_PROFILE`, `ARM_CLIENT_ID`). Use a
[secret](#secrets) for values that must stay out of the template.

**Settings.** `MERIDIAN_PACKER` names the program; `MERIDIAN_PACKER_ARGS` adds arguments to
`validate` and `build` (`-only=amazon-ebs.web`). Or `program` and `args` under
`spec.settings.packer` ([reference](../reference/plugin-settings.md#packer)).

A failure reports the step, command, exit status and Packer's output tail.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) reaches Packer only as a
variable from a private file.

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

The param becomes `${var.meridian_secret_<name>}`, declared `sensitive = true`.

The runner writes `secrets.pkrvars.json` (mode `0600`, in a `0700` directory, in memory
where possible), passed as `-var-file=<path>` to `validate` and `build`, and removed after.
A `shell-local` step's commands sit in Packer's temporary directory during the step.

A secret in a `variable` or `plugin` is refused (`unsupported-secret-place`).

Tested against a stand-in (`plugins/packer/tests/run.rs`) and real `packer` in CI
(`plugins/packer/tests/packer.rs`).

## Credentials

`spec.endpoint` names the [credential](../reference/dsl/credentials.md#reaching-a-cloud-as-a-credential)
the cloud is reached as. The Amazon plugin reads its login from the environment as the AWS SDK does, and the Google Compute plugin reads a service account key from the file `GOOGLE_APPLICATION_CREDENTIALS` names, so an `aws` or `gcp` credential reaches `packer init`, `validate` and `build` there. A `gcp` account to `impersonate` is refused: that plugin reads it from the template only.

```yaml
spec:
  target: packer
  endpoint:
    type: cloud
    credential: prod
  secrets:
    - name: aws_secret_key
      from: env
      key: PROD_AWS_SECRET_KEY
  credentials:
    - name: prod
      type: aws
      accessKeyId: AKIAIOSFODNN7EXAMPLE
      secretAccessKey: ${{ secret.aws_secret_key }}
      region: eu-west-1
  resources:
    - id: ubuntu
      type: source.amazon-ebs
      params:
        region: eu-west-1
        instance_type: t3.small
        source_ami: ami-0c7217cdde317cfec
        ssh_username: ubuntu
        ami_name: shop-web-{{timestamp}}
```

| Credential | Plain fields, as variables | Secret fields |
|---|---|---|
| `aws` | `accessKeyId` as `AWS_ACCESS_KEY_ID`, `profile` as `AWS_PROFILE`, `region` as `AWS_REGION` and `AWS_DEFAULT_REGION` | `secretAccessKey` as `AWS_SECRET_ACCESS_KEY`, `sessionToken` as `AWS_SESSION_TOKEN` |
| `gcp` | `project` as `GOOGLE_PROJECT` and `CLOUDSDK_CORE_PROJECT`; the Google Compute plugin still wants `project_id` in the source | `serviceAccountKey` in a private file named by `GOOGLE_APPLICATION_CREDENTIALS` |
| `azure` | refused: the Azure plugin reads its login from the template, which this plugin does not write one into. Sign the machine in (`az login`) and use `use_azure_cli_auth` in the source, or declare the credential `from: ambient` | |

- **In the tool's environment, and nowhere else.** The providers take a login from
  the environment, from their own files under your home directory, or from the
  machine's metadata service; Meridian holds the credential in protected memory and
  writes nothing under your home, so the environment is the way: rank 4 of the plan's
  [channels](../reference/dsl/secrets.md#handing-a-secret-to-a-tool), the last it
  allows, taken because the providers take a value no other way. The value goes from
  protected memory into the process's environment as it is spawned, and is copied
  nowhere else by Meridian. It is never on the command line, in the compiled files or
  in a file Meridian keeps. The plan line says so: `as prod (aws, region eu-west-1),
  in the tool's environment`.
- **What that means.** Another process of yours can read a running tool's environment
  (`/proc/<pid>/environ`), and everything the tool starts, such as a provider plugin,
  inherits it. That is the same exposure as exporting the variables in a shell before
  running the tool by hand, and no more.
- **A file where a provider reads one.** A `gcp` `serviceAccountKey` and an `azure`
  `certificate` are written to a file only you can read, held in memory where the
  system allows, and the environment names its path (`GOOGLE_APPLICATION_CREDENTIALS`,
  `ARM_CLIENT_CERTIFICATE_PATH`). The file is removed when the stage ends.
- **`from: ambient`** names the identity without Meridian holding any of it: the
  tool finds the login as it does today. The plain fields are still told, so an
  ambient `aws` credential with a `profile` sets `AWS_PROFILE` for the run.
- **Refused at compile**, rule `unsupported-credential`: a credential of a type the
  tool does not reach a cloud as, an `aws` `roleArn` (the SDKs assume a role through
  a profile that names it; declare that profile), and a plain field with a line break
  in it.

The compiled files hold nothing of the credential, and the plan and the run record
name it (`prod (aws, region eu-west-1)`) and nothing more. Outputs read on their own
(`meridian outputs`) are read with no credential; a backend that needs the cloud's
login to read the state is read right after the apply, inside it.

## What the plugin refuses

Each refusal names the resource, rule and line.

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

Documents in [`examples/packer/`](../../examples/packer/), compiled output in
[`plugins/packer/testdata/`](../../plugins/packer/testdata/).

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

## Testing

```console
$ cargo test -p meridian-target-packer
```

runs unit, golden-file, stand-in runner and binary tests. None needs Packer.

```console
$ cargo test -p meridian-target-packer --test packer -- --ignored
```

runs the real-`packer` tests (validate every example; null-builder builds), as the `packer`
workflow does. Re-record golden files with `UPDATE_GOLDEN=1`.
