# The `meridian` command

```
meridian validate <files>...
meridian compile  <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--out DIR]
meridian plan     <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE]
meridian apply    <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--resume [RUN]] [--on-failure stop|rollback]
meridian drift    <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE]
meridian rollback <files>... [--run RUN]
meridian status
meridian runs
meridian events   [RUN] [--type TYPE]
meridian report   [RUN] [--format FORMAT] [--out FILE] [--from FILE]
meridian unlock   <ID>
meridian plugins
meridian import   <paths>... [--from TARGET] [--name NAME] [-o DIR] [-r] [--force] [--strict]
meridian migrate  <files>... [--to APIVERSION] [--check]
meridian version
meridian module   install [--locked] | update [NAME]... | verify | list | pack [--out FILE] | push REPOSITORY
meridian rules check --hierarchy FILE [--rules FILE] [--require-rules] [--data FILE] [--set KEY=VALUE]
```

The files given to one command are one workflow. A file may hold several
documents separated by `---`. Infrastructure documents run before ResourceSets
([why](../concepts/two-stage-architecture.md)), and a document runs after every
document it [imports](dsl/outputs-and-hand-offs.md#imports) from.

A file can also be one inside a [module](modules.md) the project has pinned,
written `@<module>:<path>`:

```
meridian plan site.yaml @acme/nginx:documents/proxy.yaml --data @acme/nginx:data/defaults.yaml
```

This works wherever a document or a `--data` file is given. The module must be
in the project's `meridian.lock` and already fetched: `validate`, `compile`,
`plan` and `apply` never fetch anything, and they check each module against
its pinned hash before reading it.

## Commands

### `validate`

Checks the documents and loads no plugin: required fields, unique ids,
dependencies that exist and do not form a cycle, conditions that are written in
the condition language, and hand-offs between documents that name a real
document and output. Errors give the file, line and field. A cycle is reported
with the resources in it:

```
site.yaml:12:7: spec.resources[1].dependsOn: dependency cycle among 2 resources: key -> lock -> key
```

It does not know what a target can express, so it cannot say whether the
target takes the document's kind, its `targetVersion` or its resource types;
`compile` does. [Which check happens when](dsl/index.md#which-check-happens-when)
has the full list.

### `compile`

Compiles each document into its target's own files and changes nothing. With
`--out DIR` the files are written to `DIR/<document name>/`. Without it, the
command only reports whether each document compiles, with any warnings.

A ResourceSet whose host is an output that has not been produced yet is reported
as deferred.

Before a document reaches its plugin, Meridian settles what is its own to
settle, and refuses the document if it cannot:

- the document's kind is one the target's category takes
  ([the pairing](dsl/index.md#the-document));
- the target compiles for the document's
  [`targetVersion`](dsl/index.md#targetversion), if it sets one;
- every [`when`](dsl/index.md#when-and-runtimewhen) is decided from the `--data`
  files, and the resources whose condition is false are left out.

`plan` and `apply` compile first, so the same holds for them.

### `plan`

Compiles each document and asks its tool what an apply would change, using the
tool's own preview. Changes are listed by the ids the document uses:

```
  + create ...
  ~ update ...
  - delete ...
  ! replace ...
```

When a tool cannot say what would change, the plan is reported as unknown
instead of empty.

A plan runs each tool, so it takes the [state lock](../concepts/run-safety.md#one-run-at-a-time)
and is recorded like an apply.

With `--policy`, every stage is put to the [policy](policy.md), and its
warnings and its decision are shown above the plan. A stage the policy denies
is reported as failed and the command exits `1`; the stages after it are
still planned, so one plan shows every denial.

### `apply`

Compiles and applies each document in order, stopping at the first failure. A
stage's outputs are shown and handed to the stages that asked for them.
Values a tool marks sensitive are shown as `(sensitive)`.

An apply holds the [state lock](../concepts/run-safety.md#one-run-at-a-time)
while it runs, so a second one against the same state is refused, and it is
[recorded](../concepts/run-safety.md#the-record) step by step.

When a stage fails, what its tool had already changed is listed above the
failure, the stages after it are not run, and the command says how to
continue:

```
web (ResourceSet, target ansible on 203.0.113.10)
  ~ update nginx_conf
  FAILED: the play failed (ran `ansible-playbook ...`, exit status 2)
run 2026-11-17T09-14-03Z-0cc7 failed. Once the cause is fixed, `meridian apply --resume` with the same documents continues from the stage that failed
```

`--resume` continues the last apply that failed or was interrupted, and
`--resume <run>` a named one. Stages that run already applied, from documents
that still compile to the same artifact, are left alone; the rest are applied.
[Picking up where a run stopped](../concepts/run-safety.md#picking-up-where-a-run-stopped)
has the rules.

With `--policy`, the [policy](policy.md) is asked about every stage before
the first one is applied. If it denies any of them, nothing is applied:

```
network (Infrastructure, target terraform)
  FAILED: the policy does not allow this change:
      - network: aws_db_instance.main would be a replace in prod
web (ResourceSet, target ansible)
  not run, because Infrastructure "network" failed
run 2026-11-17T09-14-03Z-0cc7 was stopped by its policy before anything was applied
```

With `--on-failure rollback`, a failed apply instead
[puts back](../concepts/run-safety.md#putting-a-run-back) every stage it
changed, the failed one first and then the ones before it, by applying what
each was last applied from successfully. The command still exits `1`: the
apply failed.

### `drift`

Checks whether what was applied is still what is there, and changes nothing.
Each document is compiled and its tool asked for a preview, and each stage is
reported as `in sync`, `DRIFTED`, `pending` (the document changed since the
last apply, so the differences are changes waiting to be applied), `unknown`
(the tool cannot preview) or `never applied`.
[Finding drift](../concepts/run-safety.md#finding-drift) has the rules.

```
web (ResourceSet, target ansible on 203.0.113.10)
  ~ update sshd_config
  DRIFTED: 1 difference from what was applied
drift: 1 of 2 stages drifted
```

It exits `2` when a stage drifted, so a scheduled job can tell drift from a
clean check (`0`) and from a check that could not be made (`1`).

### `rollback`

Puts back what the last apply changed: each stage that apply handed to a tool
is applied again from the artifact of its last successful apply before it,
last stage first. `--run RUN` names another apply. Give it the files the
apply was given.

A stage that cannot be put back is reported as `NOT ROLLED BACK` with the
reason, and the command exits `1`.
[Putting a run back](../concepts/run-safety.md#putting-a-run-back) lists the
cases, among them a stage that was applied for the first time, which has
nothing to go back to.

### `status`

Shows the current or last run: what was asked, who ran it, how each step
stands, and whether the state is locked and by whom.

```
run 2026-11-17T09-14-03Z-0cc7: apply, failed
  command:  meridian apply site.yaml
  started:  2026-11-17T09:14:03.204Z by mike@build-07
  finished: 2026-11-17T09:15:11.910Z
  network (Infrastructure, target terraform): succeeded, 4 changes
  web (ResourceSet, target ansible on 203.0.113.10): failed (tool-failed), 2 changes
not locked
```

A run that was killed is shown as `interrupted`.

### `runs`

Lists every recorded run, oldest first: its id, whether it was a plan or an
apply, how it ended, and which run it resumed.

```
2026-11-17T09-14-03Z-0cc7  apply    failed     2 steps, 1 failed
2026-11-17T09-21-40Z-1f42  apply    succeeded  2 steps, resumes 2026-11-17T09-14-03Z-0cc7
```

### `events`

Prints a run's [events](state.md#events), one JSON object per line; the last
run's when no run is named. `--type` keeps only events of that type, takes a
family as `'step.*'`, and may be repeated.

```
meridian events --type step.failed --type 'resource.*'
```

### `report`

Writes a [report](reporting.md) of a recorded run, the last one when no run
is named: `--format` is `json`, `junit`, `markdown` (the default) or `html`,
and `--out` names a file to write instead of standard output. `--from FILE`
reports on a file of events, as `--events json` wrote them, and not on the
state; `-` reads standard input.

```
meridian report --format junit --out meridian.xml
meridian apply site.yaml --events json | meridian report --from - --format html --out run.html
```

### `unlock`

Removes the state lock a run left behind when it was killed. It takes the
lock's id, as `status` and the refused run show it, and refuses any other, so
the lock removed is the one you looked at. Make sure the run that held it is
really gone first: removing the lock of a run that is still going lets a
second one collide with it.

### `plugins`

Lists every plugin Meridian can find: its category, what it can do, the resource
types it takes, the versions of its tool it compiles for (when it declares any),
and where its binary is.

```
dsc
  category:  configuration-management
  can:       compile, plan, apply, import
  resources: <Module>/<Resource>, runner
  versions:  2, 3
  imports:   PowerShell DSC configurations (*.ps1)
  imports:   DSC v3 configuration documents and DscPipelineRunner projects (*.dsc.yaml, *.dsc.yml, *.dsc.json, *.dsc.config.yaml, *.dsc.config.yml, *.dsc.config.json, Datum.yml, *.yaml, *.yml, *.json)
  sdk:       0.1.0 (protocol 2)
  binary:    /usr/local/bin/meridian-target-dsc
```

`inputs` in the `can` line says the plugin hands its tool the values a
document's [`spec.imports`](dsl/outputs-and-hand-offs.md#imports) brings from
other documents.

The category decides which kind of document the plugin is handed, and the
versions decide which `targetVersion` a document may ask it for. A plugin that
can read its tool's existing configuration lists `import` under `can`, and the
file names each of its formats claims under `imports`.

### `import`

Turns a tool's existing configuration into Meridian documents. It only reads:
no tool is run and nothing but the documents is written.

```
meridian import ./compose.yaml                      # print the document
meridian import ./infra --from terraform -o imported  # write imported/<name>.yaml
meridian import ./playbooks -r -o imported --strict  # fail if anything was lost
```

| Option | Meaning |
|---|---|
| `PATHS...` | the files to import, or directories holding them. A directory gives the files in it that the plugin's formats claim |
| `--from TARGET` | the plugin whose tool the files belong to. Left out, it is worked out from the file names, and the import stops and asks when the names fit more than one plugin (`main.tf` is both Terraform's and OpenTofu's) |
| `--name NAME` | what to call the imported document. Defaults to the name of the directory or file, or to a name the source itself gives |
| `-o DIR`, `--out DIR` | write each document to `DIR/<name>.yaml`. Left out, the documents are printed to standard output |
| `-r`, `--recursive` | look in the directories inside a directory too |
| `--force` | replace a document that already exists under `--out` |
| `--strict` | exit `1` when anything was changed or not translated. The documents are still written |

The report is written to standard error: the documents and how many resources
each holds, then what was **not translated**, what was **changed**, and any
**notes**. Everything in the first two groups is also a comment in the document
that starts `TODO(meridian-import)`, beside the resource it is about. Nothing a
source says is left out without one.

Every document is validated before it is written, exactly as `meridian
validate` would.

[Importing existing configuration](../operations/importing.md) walks through an
import, says what each plugin reads, and what an imported document does and
does not promise.

`sdk` is the SDK release the plugin was built against, and `protocol` is the
protocol version `meridian` and the plugin agreed on when it started. A plugin
whose protocol or SDK this `meridian` cannot work with is not listed: the
command fails and says which side is behind.
[Compatibility](compatibility.md) has the rules.

### `migrate`

Rewrites documents from an older language version (`apiVersion`) to a newer
one, in place.

```
meridian migrate site.yaml modules/*.yaml
meridian migrate site.yaml --to meridian/v1
meridian migrate site.yaml --check
```

- `--to` names the version to move to. Without it, the newest version this
  build supports.
- `--check` changes nothing and exits `1` if any file would change. Use it in
  CI to keep an estate on the current version.

Each file is reported on its own line:

```
site.yaml: already at meridian/v1
```

A file is rewritten as text, so comments, key order and layout are kept. Only
an `apiVersion` at the top level of a document is changed; an `apiVersion`
inside a resource's `params`, such as a Kubernetes object's, is left alone. A
file is rewritten whole or not at all.

`migrate` never guesses. Where a change between two versions needs a decision
only you can make, it lists those decisions for that file, leaves the file as
it was, and exits `1`. It never moves a document to an older version.

`meridian/v1` is the only language version so far, so today the command
reports that every Meridian document is already current. It is here now so
that the release that introduces `meridian/v2` ships with its migration.

### `version`

Shows every version this build carries. One number cannot answer "which
version of Meridian": the core, the language, the plugin SDK and the plugin
protocol each move on their own.

```
meridian 0.1.0
  language         meridian/v1  supported
  plugin sdk       0.1.0
  plugin protocol  2
  lockfile format  1
```

A language version is `supported`, `deprecated since meridian <release>` or
`removed in meridian <release>`. `plugin protocol` is the range this build
speaks, such as `2` or `2 to 3`. `lockfile format` is the format of
[`meridian.lock`](modules.md#the-lockfile) this build writes. `meridian --version`
prints the first line only. [Compatibility](compatibility.md) explains each.

### `rules check`

Loads the layers of a hierarchy and checks them against its
[compile rules](compile-rules.md): policies such as "a more specific layer may
not change a variable or a secret a less specific layer set". It compiles
nothing and loads no plugin.

```
$ meridian rules check --hierarchy config/hierarchy.yaml --set env=prod --set node=web01
error: compile rules: 1 violation; nothing was compiled
  - [meridian/protect-variables] layer "node/web01.yaml" (team) changes "variables.Region", which layer "common.yaml" (organization) set
```

The rules file is `rules.yaml` beside the hierarchy file, or the file given
with `--rules`. Every violation is listed, each naming the rule, the layer,
its group and the key, and never a value. With no rules file nothing is
checked, unless `--require-rules` is given, which makes that an error.
`--data FILE` and `--set KEY=VALUE` mean what they mean on `compile`: values
merged over the hierarchy, `--set` last. Each is checked as a layer of the
group `(command line)`, and a layer path's `%{KEY}` reads the `--set` value. [The reference](compile-rules.md#running-the-check) has the options
and the output in full.

## Options

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--plugin-dir DIR` | `MERIDIAN_PLUGIN_DIR` (separated by `:`) | the directory `meridian` is in | where to look for plugins; may be repeated |
| `--state-dir DIR` | `MERIDIAN_STATE_DIR` | `.meridian` | each tool's working files, fetched [modules](modules.md#where-modules-are-kept), and with the local backend Meridian's own record |
| `--backend NAME` | `MERIDIAN_BACKEND` | `local` | where Meridian's own record (the lock, the run-state, the runs) is kept: `local`, or a [backend program](state.md#writing-a-backend) |
| `--backend-config KEY=VALUE` | | | a setting for the backend; may be repeated |
| `--lock-timeout DURATION` | | `0s` | `plan`, `apply`, `drift`, `rollback`: how long to wait for another run to release the lock, as in `30s`, `5m`, `1h`. By default a locked state is refused at once |
| `--resume [RUN]` | | | `apply` only: continue the last apply, or the run named |
| `--on-failure ACTION` | | `stop` | `apply` only: `stop` leaves what was applied in place; `rollback` puts back every stage the run changed |
| `--run RUN` | | the last apply | `rollback` only: the apply to put back |
| `--policy PATH` | | no policy | `plan`, `apply`: a [policy](policy.md) file or directory every stage is checked against before anything is applied; may be repeated |
| `--policy-engine NAME` | `MERIDIAN_POLICY_ENGINE` | `opa` | `plan`, `apply`: the program `meridian-policy-<NAME>` that reads the policies |
| `--events json` | | | `compile`, `plan`, `apply`, `drift`, `rollback`: write every [event](state.md#events) to standard output as it happens, one JSON object per line. The report for people moves to standard error |
| `--report FORMAT=FILE` | | | `compile`, `plan`, `apply`, `drift`, `rollback`: write a [report](reporting.md#--report) of the run when it ends, as `json`, `junit`, `markdown` or `html`; may be repeated |
| `--sink NAME` | `MERIDIAN_SINKS` (separated by `,`) | | the same commands: send every event, as it happens, to a [sink](reporting.md#sinks); may be repeated |
| `--sink-config SINK.KEY=VALUE` | | | a setting for a sink; may be repeated. `on` chooses the event types and `required=true` fails the run when the sink fails |
| `--sink-timeout DURATION` | | `30s` | how long a sink has to finish sending after the run's last event |
| `--keep-runs N` | | `50` | `plan`, `apply`, `drift`, `rollback`: how many runs' records to keep |
| `--out DIR` | | | `compile` only: where to write the compiled files |
| `-l SELECTOR`, `--selector SELECTOR` | | every document | `compile`, `plan`, `apply`: run only the documents whose `metadata.labels` match, as in `tier=web,env!=prod`. May be repeated; every requirement must hold. [Labels and selectors](dsl/index.md#labels-and-selectors) |
| `--data FILE` | | no data | `compile`, `plan`, `apply`: a YAML or JSON mapping of values that [`when`](dsl/index.md#when-and-runtimewhen) conditions and [interpolations](dsl/interpolation.md) read as `data.<key>`. May be repeated; a later file's key replaces an earlier file's |
| `--set KEY=VALUE` | | | `compile`, `plan`, `apply`: one value for `when` conditions and interpolations, applied after every `--data` file, so it wins. `KEY` may be dotted to set one key inside a mapping (`region.name=eu-west-1`). The value is typed as YAML types it. May be repeated. See [giving the data](dsl/conditions.md#giving-the-data) |

### Running part of a workflow

```
meridian apply site.yaml -l tier=web
```

Only the documents labelled `tier: web` run. The others are still loaded and
checked, and a selected document can still take its host from an unselected
one's last apply. A selector that matches no document is an error.

### Deciding conditions

```
meridian plan site.yaml --data common.yaml --data prod.yaml
```

A resource with `when: "data.env != 'prod'"` is in the plan or not, depending on
what the data files say `env` is. A condition that reads a key no file sets is
an error; nothing is guessed.

The same values are what an [interpolation](dsl/interpolation.md) reads:
`${{ data.env }}` inside a value is replaced before the document is compiled.

Every decision is printed under its stage, and a document its own `spec.when`
leaves out is reported as skipped. Neither is a failure:

```
site (ResourceSet, target ansible on shop-dev.example.com)
  when: 2 removed
    resource "site_cert": cond.needs_tls is false
    "site_conf" depends on "site_cert": cond.needs_tls is false
  compiled 2 files into build/site
monitoring (Infrastructure, target kubernetes)
  skipped: its when (data.monitoring.enabled ?? false) is false
```

A label can be interpolated too, and `-l` matches the value it comes to, so
the data is read before the selector is applied.

### Secrets

A document's [secrets](dsl/secrets.md) are fetched by `plan` and `apply` only,
each just before the stage that declares it, and wiped from memory when that
stage ends. `validate` and `compile` never read a provider: `compile` writes a
secret's name where its value would go, as `(secret db_password)`.

A secret's value is in nothing a command prints: plans, the tool's log lines,
errors and the run's state show `(sensitive)`. The process also turns off its
own core dumps before it reads anything
([how a secret is held](../concepts/secrets-in-memory.md)).

### `module`

Fetches, pins and checks the [modules](modules.md) that the `meridian.yaml` in
the current directory requires.

| Command | Does |
|---|---|
| `meridian module install` | Resolves the requirements, fetches what is missing, writes `meridian.lock`. With `--locked`, fails if the lockfile would change |
| `meridian module update [NAME]...` | Moves the named modules, or all of them, to the newest version that still fits |
| `meridian module verify` | Checks every pinned module against its hash; exits `1` if any differs |
| `meridian module list` | Lists the pinned modules, their versions and sources |
| `meridian module pack [--out FILE]` | Writes the module in the current directory as the archive a registry stores |
| `meridian module push REPOSITORY [--insecure]` | Publishes the module in the current directory to an OCI registry under its version. A version that is already published is never replaced with other content |

Only `install`, `update` and `push` use the network. Each command, its output and its
errors are described in [Modules](modules.md#commands).

### A deprecated language version

When a run reads documents written in a deprecated `apiVersion`, it prints one
warning for the whole run, however many documents there are, and carries on:

```
warning: 14 documents, the first being "web", use apiVersion "meridian/v1", which is deprecated since meridian 2.0.0 and will stop being read; run `meridian migrate --to meridian/v2` to move to meridian/v2
```

No version is deprecated today.

## Exit status

`0` when everything asked for succeeded. `1` when a document was invalid, did
not compile, a tool failed, a policy denied a change or could not be asked, a
stage could not be rolled back, or the run was
refused because the state is locked or there was nothing to resume or roll
back, or a [sink](reporting.md#sinks) marked `required` failed. `2` for a mistake in the command line itself, and from `drift` when a
stage has drifted.

`import` exits `0` when it wrote its documents, whatever it reported about
them; `1` when the source could not be read or no plugin imports it, and, with
`--strict`, when anything was changed or not translated.

## Plugins and their tools

A plugin is a separate program named `meridian-target-<name>`. Each one runs its
tool as you would by hand, with the credentials and configuration the tool
already has. Every plugin reads a few environment variables that name a
different binary or add arguments to every call; they are listed on each
[target's page](../targets/ansible.md#running).
