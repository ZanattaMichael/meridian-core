# The `meridian` command

```
meridian validate <files>...
meridian compile  <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--out DIR]
meridian plan     <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--strict-secrets] [--detailed-exitcode]
meridian apply    <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--strict-secrets] [--resume | --resume=RUN] [--on-failure stop|rollback] [--stage-timeout DURATION]
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
meridian module   install [--locked] | update [NAME]... | verify | list | sign --key FILE | pack [--out FILE] | push REPOSITORY
meridian secret check <files>... | set NAME <files>...
meridian rules check --hierarchy FILE [--rules FILE] [--require-rules] [--data FILE] [--set KEY=VALUE]
meridian fmt      <paths>... [--check] [--keep-order]
meridian lint     <paths>... [--data FILE] [--set KEY=VALUE] [--state-dir DIR] [--fix] [--strict] [--json]
meridian schema   [--target TARGET] [-o FILE] [--state-dir DIR]
meridian schema   sync <paths>... [--target TARGET] [--data FILE] [--set KEY=VALUE] [--state-dir DIR] [--check] [--no-lock]
meridian schema   status [--state-dir DIR] [--json]
meridian schema   clean [TARGET]... [--state-dir DIR]
meridian lsp
```

Every command also takes `--output text|json`: with `json` it prints one JSON
object instead of text, for a pipeline or a wrapping tool.
[Machine-readable output](cli-json.md) describes every object.

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

A plan exits `0` whether or not an apply would change anything. With
`--detailed-exitcode` it exits `2` when an apply would do something (a stage
with changes, a tool that cannot preview, or a stage deferred until an
infrastructure output exists), `0` when every stage is in sync, and `1` on an
error, as `drift` does; a pipeline can then ask "is there anything to apply?"
without reading the plan. With `--output json` the plan is one object a tool
can read ([the run object](cli-json.md#compile-plan-apply-drift-rollback)).

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
run 2026-11-17T09-14-03Z-0cc7 failed. Once the cause is fixed, `meridian apply --resume site.yaml` continues from the stage that failed
```

`--resume` continues the last apply that failed or was interrupted, and
`--resume=<run>` a named one. The run's id is joined to the flag with `=`, so
that a file after `--resume` is always a file; `--resume` may come before or
after the documents. Stages that run already applied, from documents
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

Ctrl-C [stops the run](../concepts/run-safety.md#stopping-a-run): the tool is
stopped, the stage is recorded as `interrupted`, the lock is released and the
command exits `130`. A second Ctrl-C exits at once. `--stage-timeout 30m`
stops a stage whose tool has run that long and records it as `timed-out`;
without it a tool may take as long as it takes. Both apply to `plan`,
`drift` and `rollback` as well.

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
the keys of a document's `spec.settings` it reads (when it takes any), the
[refusal rules](refusal-rules.md) it has of its own (when it declares any),
where its binary is, and what checking its signature found.

```
dsc
  category:  configuration-management
  can:       compile, plan, apply, import
  resources: <Module>/<Resource>, runner
  versions:  2, 3
  settings:  credential, engine, target
  imports:   PowerShell DSC configurations (*.ps1)
  imports:   DSC v3 configuration documents and DscPipelineRunner projects (*.dsc.yaml, *.dsc.yml, *.dsc.json, *.dsc.config.yaml, *.dsc.config.yml, *.dsc.config.json, Datum.yml, *.yaml, *.yml, *.json)
  sdk:       0.1.0 (protocol 2)
  binary:    /usr/local/bin/meridian-target-dsc
  signature: verified, signed by key 780001b5de1d7691
```

`inputs` in the `can` line says the plugin hands its tool the values a
document's [`spec.imports`](dsl/outputs-and-hand-offs.md#imports) brings from
other documents.

The category decides which kind of document the plugin is handed, the
versions decide which `targetVersion` a document may ask it for, and the
settings keys decide what a document may write under
[`spec.settings`](dsl/index.md#settings); a plugin that lists none takes none. A plugin that
can read its tool's existing configuration lists `import` under `can`, and the
file names each of its formats claims under `imports`.

`meridian plugins --settings` adds, under each plugin, every setting its
runner declares: the short name to use under `spec.settings` in
`meridian.yaml`, the environment variable, the default and what it does.
`meridian plugins --rules` adds each plugin's own [refusal rules](refusal-rules.md),
with what each refuses and what to write instead; a plugin that has none says
so, and refuses only under the general rules of that page.

```
ansible
  ...
  settings:
    program (MERIDIAN_ANSIBLE_PLAYBOOK, default "ansible-playbook")
      The `ansible-playbook` program to run, when it is not on `PATH`.
    args (MERIDIAN_ANSIBLE_ARGS, default none)
      Extra arguments for every run, split like a shell command line: ...
```

The same list for every shipped plugin is the
[plugin settings reference](plugin-settings.md). When the current directory
holds a `meridian.yaml`, every command that starts plugins reads its
`spec.settings` and hands each target its own; a setting a plugin does not
declare refuses to start it, naming the ones it does.

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

### `secret check`

Says, for every [secret](dsl/secrets.md) the documents declare, where it comes
from and whether that place would answer. It reads no value: a file is looked
at and not opened, a command is found and not run.

```
$ meridian secret check site.yaml
site (site.yaml)
  db_password   env    ok: the environment variable SITE_DB_PASSWORD is set
  deploy_token  file   REFUSED: the file /run/secrets/deploy-token has mode 0644, which lets others than its owner read it; make it 0600, or set allowWideMode: true on the secret if that is intended
  vault_token   exec   not checked: the command vault is there, and is only run by plan and apply
  extra         env    absent, optional: the environment variable SITE_EXTRA is not set
error: 1 secret would not be found by plan or apply
```

| State | Meaning | Fails the check |
|---|---|---|
| `ok` | the place holds something a fetch would read | no |
| `not checked` | the command exists; only running it tells what it prints | no |
| `absent, optional` | nothing is there, and the secret is `optional: true`, so it reads as `null` | no |
| `MISSING` | nothing is there | yes |
| `REFUSED` | the place is there and a fetch would refuse it, or the command was not found | yes |
| `UNAVAILABLE` | the store cannot be used on this machine: another operating system, no session bus, its tool not installed | yes |

A secret with [several places](dsl/secrets.md#several-places-for-one-secret)
is shown with the first place that would answer, or with every place's reason
when none would. The Secret Service and the keychain are `not checked`, since
asking them can open a prompt.

A credential declared [`from: ambient`](dsl/credentials.md#from-ambient) is
looked for too, where the tool itself would look, so that a job whose
identity is missing stops here and not an hour later inside the tool:

```
$ meridian secret check site.yaml
site (site.yaml)
  db_password  env    ok: the environment variable SITE_DB_PASSWORD is set
  deployer     aws    ambient, ok: the environment holds AWS_WEB_IDENTITY_TOKEN_FILE and AWS_ROLE_ARN (web identity, such as a pipeline's OIDC token)
```

An `aws`, `azure` or `gcp` credential is looked for in the cloud's own
order: the environment, the files a login leaves behind, and last the
machine's identity endpoint, which is asked one question with a one-second
timeout. An `ssh` credential is the agent `SSH_AUTH_SOCK` names, a
`kubeconfig` one the file `KUBECONFIG` names; any other type is
`not checked`. A credential that would not be found is `MISSING`, or
`REFUSED` when what is set points at a file that is not there, and fails the
check. [The places, type by type](../operations/running-in-a-pipeline.md#checking-the-jobs-identity-first).

It takes `--state-dir`, for a file written `@<module>:<path>`, and loads no
plugin.

### `secret set`

Stores a value for the [secret](dsl/secrets.md#storing-a-secret) `NAME`, as
the documents declare it, in the first place it names that meridian can
write and this machine has: the kernel keyring (`from: keyring`) or the
Secret Service (`from: secret-service`). The value is read from standard
input, or typed at a prompt that does not show it, and is never an argument.

```
$ meridian secret set deploy_token site.yaml
Value for deploy_token (not shown):
stored deploy_token in the kernel keyring, key meridian:deploy-token
$ printf '%s' "$TOKEN" | meridian secret set deploy_token site.yaml
stored deploy_token in the kernel keyring, key meridian:deploy-token
```

From a pipe the value is everything read without its last line ending; at a
terminal it is the line typed. An empty value stores nothing. A secret that
names no writable place, or one that is not declared, is refused and the
value is not read. It takes `--state-dir` as `secret check` does.

### `fmt`

Rewrites documents in the one standard layout: two-space indentation, the
language's own field order, no trailing spaces. It changes how a document is
laid out and never what it says, and it checks that before writing. `--check`
changes nothing, lists the files that are not formatted and exits `1` if there
are any. `--keep-order` leaves fields in the order they were written. `-` reads
standard input and writes standard output.

A path may be a directory, which is searched for Meridian documents. Full
description: [The formatter](../tooling/formatter.md).

### `lint`

Reports everything `validate` and `compile` would refuse, for every document
under the paths given, and then things that compile and are probably mistakes.
Each document is compiled by its target's plugin in memory; nothing is written
and no tool is run.

```console
$ meridian lint stacks/
stacks/web.yaml:16:9: error: param "mode" of a file must be text, not a number; write it quoted, so it is not read as a number: "420" [invalid-param]
stacks/web.yaml:16:15: warning: mode 0644 is read as a number, not as permissions; write it as a quoted string [unquoted-mode]
stacks/web.yaml:35:21: error: spec.resources[3].dependsOn[0].resource: resource "warm" depends on unknown resource "wbe" [invalid-document]
2 errors, 1 warning, 0 notes
```

`--fix` applies the fixes that are safe. `--strict` fails on warnings as well
as errors. `--json` prints one JSON object per finding. `--data` and `--set`
are as for `compile`; without them a document that needs data is checked
with every `when` treated as true. `--state-dir` is where the project's
fetched modules and [synced tool schemas](../tooling/schema-sync.md) are,
so a tool's own params are checked too. Full
description: [The linter](../tooling/linter.md); the rules:
[Lint rules](lint.md).

### `schema`

Prints a JSON Schema (draft 7) of the document language and of the resource
types of every plugin found, for editors that validate YAML against a schema.
The tool schemas cached under `--state-dir` are written in too. `--target`
writes one target's rules only; `-o` writes to a file. Full
description: [The JSON Schema](../tooling/json-schema.md).

### `schema sync`, `schema status`, `schema clean`

`schema sync` asks each target's installed tool, through its plugin, for the
schemas of the resource types the documents under the paths use, caches them
under `--state-dir` (`.meridian/schemas/<target>.json`) and records the
versions they came from in `meridian.lock`. Editors, `lint` and `schema`
read the cache, so a tool's own types (`aws_instance`, `apps/v1/Deployment`)
are completed and checked without the editor ever running a tool. `--target`
syncs one target; `--check` changes nothing and exits `1` if any cache is
missing or is not what the lock records; `--no-lock` leaves the lock alone.

`schema status` shows what is cached for each target and how it stands
against the lock, as text or with `--json` one object per line. `schema
clean` removes the cache of the targets named, or of every target, and their
records in the lock.

Full description: [Tool schemas](../tooling/schema-sync.md).

### `lsp`

Runs the language server on standard input and output. An editor starts it;
there is nothing to see when it is run by hand. It takes the plugin options
(`--plugin-dir`, `--plugin-signatures`, `--trusted-key`) like every command,
plus `--trust-editor-plugin-dirs` to load unsigned plugins from directories the
editor names. Full description:
[The language server](../tooling/language-server.md).

### `signing`

Makes signing keys, signs plugin binaries and checks their signatures. It is
for people who ship a plugin, or who want to check one without starting it;
[Plugin signatures](plugin-signatures.md) is the guide.

```
meridian signing keygen --secret acme.key --public acme.pub
meridian signing sign --key acme.key meridian-target-acme
meridian signing verify --trusted-key acme.pub meridian-target-acme
```

| Command | Does |
|---|---|
| `signing keygen --secret FILE --public FILE` | makes a new key pair. Neither file may exist already |
| `signing sign --key FILE BINARY...` | writes `<BINARY>.sig` beside each binary |
| `signing verify [--trusted-key FILE]... BINARY...` | prints what each binary's signature amounts to; exits non-zero if any is not verified |

## Options

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--output FORMAT` | `MERIDIAN_OUTPUT` | `text` | `text` for a person, or `json`: one JSON object per command on standard output, the report for people on standard error. [Machine-readable output](cli-json.md) has every object; may be given to any command |
| `--detailed-exitcode` | | | `plan` only: exit `2` when an apply would do something, `0` when nothing would change, `1` on an error |
| `--plugin-dir DIR` | `MERIDIAN_PLUGIN_DIR` (separated by `:`, or `;` on Windows) | the directory `meridian` is really in, following a link to it | where to look for plugins; may be repeated |
| `--plugin-signatures POLICY` | `MERIDIAN_PLUGIN_SIGNATURES` | `require` in a released `meridian`, `off` in one built from source | what to do about [plugin signatures](plugin-signatures.md): `off` does not look, `warn` starts an unsigned or wrongly signed plugin and says so, `require` starts only plugins signed by a trusted key |
| `--trusted-key FILE` | `MERIDIAN_TRUSTED_KEYS` (separated by `:`, or `;` on Windows) | the key a released `meridian` was built with | a public key whose signatures on plugins and on [modules](modules.md#signatures) are trusted, or a directory of `*.pub` files; may be repeated, and given to any command |
| `--module-signatures POLICY` | `MERIDIAN_MODULE_SIGNATURES` | `off` | what to do about [module signatures](modules.md#signatures): `off` does not look, `warn` uses an unsigned or wrongly signed module and says so, `require` uses only modules signed by a trusted key. Applies to modules fetched from git or a registry, when they are installed and every time they are used; may be given to any command |
| `--state-dir DIR` | `MERIDIAN_STATE_DIR` | `.meridian` | each tool's working files, fetched [modules](modules.md#where-modules-are-kept), and with the local backend Meridian's own record |
| `--backend NAME` | `MERIDIAN_BACKEND` | `local` | where Meridian's own record (the lock, the run-state, the runs) is kept: `local`, or a [backend program](state.md#writing-a-backend) |
| `--backend-config KEY=VALUE` | | | a setting for the backend; may be repeated |
| `--lock-timeout DURATION` | | `0s` | `plan`, `apply`, `drift`, `rollback`: how long to wait for another run to release the lock, as in `30s`, `5m`, `1h`. By default a locked state is refused at once |
| `--resume`, `--resume=RUN` | | | `apply` only: continue the last apply, or the run named (joined with `=`) |
| `--on-failure ACTION` | | `stop` | `apply` only: `stop` leaves what was applied in place; `rollback` puts back every stage the run changed |
| `--stage-timeout DURATION` | | none | `plan`, `apply`, `drift`, `rollback`: stop a stage whose tool has run this long, as in `30m` or `2h`, and fail the run as `timed-out`. By default a tool may take as long as it takes |
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
| `--strict-secrets` | `MERIDIAN_STRICT_SECRETS` | off | `plan`, `apply`: stop a stage whose secret cannot be [locked in memory](#secrets), instead of warning once and going on |
| `--set KEY=VALUE` | | | `compile`, `plan`, `apply`: one value for `when` conditions and interpolations, applied after every `--data` file, so it wins. `KEY` may be dotted to set one key inside a mapping (`region.name=eu-west-1`). The value is typed as YAML types it. May be repeated. See [giving the data](dsl/conditions.md#giving-the-data) |
| `--annotations MODE` | `MERIDIAN_ANNOTATIONS` | `auto` | any command: inside a [pipeline engine](#inside-a-pipeline-engine), also write each problem in the engine's annotation syntax and the run's report as the job's summary. `auto` does so when standard output is not a terminal, `on` does so regardless, `off` never does |

### Inside a pipeline engine

Every engine sets one variable in the jobs it runs: `GITHUB_ACTIONS`,
`TF_BUILD` (Azure Pipelines), `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, any
`ARGO_*` (Argo Workflows) or any `TEKTON_*`. When one is set and standard
output is not a terminal, `meridian` also writes for the engine:

- each problem in the engine's own annotation syntax, on standard error
  beside the error for a person: a refused document at its file, line and
  column, a failed step at its document, a refused run with no place. GitHub
  Actions shows it on the pull request; Azure Pipelines as an issue of the
  job. GitLab CI, Jenkins, Argo and Tekton have no such syntax, and get the
  log;
- the run's report, the same Markdown that `--report markdown=FILE` writes,
  as the job's summary: appended to `$GITHUB_STEP_SUMMARY`, uploaded with
  `##vso[task.uploadsummary]` on Azure, or handed to `buildkite-agent
  annotate` on Buildkite when the agent is on the `PATH`. A command that
  failed before it had a run summarises its problems instead.

```
$ GITHUB_ACTIONS=true meridian validate site.yaml 2>&1 | cat
error: site.yaml:34:7: spec.resources[0]: unknown field "colour"
::error file=site.yaml,line=34,col=7,title=Meridian%3A validate::spec.resources[0]: unknown field "colour"
```

Standard output is untouched, so `--events json` and `--output json` work as
they do anywhere. Nothing is coloured, inside an engine or out of it, and
nothing is masked, because no value is ever printed. `--annotations off`
turns it off; `--annotations on` keeps it on when standard output is a
terminal, which is how Buildkite runs a job.
[Running Meridian in a pipeline](../operations/running-in-a-pipeline.md)
has a snippet per engine and the table of what each engine shows.

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

A secret is locked in memory so that it cannot be written to swap. Where the
machine does not allow that (the limit on locked memory, `ulimit -l`, is too
small), the run says so once and goes on:

```
site (ResourceSet, target ansible on web01)
  warning: secrets: the secret "db_password" could not be locked in memory and may reach swap. Raise the limit on locked memory (ulimit -l, or LimitMEMLOCK= for a service). --strict-secrets makes this an error
```

With `--strict-secrets` the stage stops instead, before its tool is run.
[`meridian secret check`](#secret-check) says where each declared secret
comes from and whether it would be found, without reading any.

### `module`

Fetches, pins and checks the [modules](modules.md) that the `meridian.yaml` in
the current directory requires.

| Command | Does |
|---|---|
| `meridian module install` | Resolves the requirements, fetches what is missing, writes `meridian.lock`. With `--locked`, fails if the lockfile would change |
| `meridian module update [NAME]...` | Moves the named modules, or all of them, to the newest version that still fits |
| `meridian module verify` | Checks every pinned module against its hash, and its signature when `--module-signatures` says to; exits `1` if any fails |
| `meridian module list` | Lists the pinned modules, their versions and sources |
| `meridian module sign --key FILE` | Signs the module in the current directory, writing `meridian.sig` in it |
| `meridian module pack [--out FILE]` | Writes the module in the current directory as the archive a registry stores |
| `meridian module push REPOSITORY [--insecure] [--credential NAME]` | Publishes the module in the current directory to an OCI registry under its version. A version that is already published is never replaced with other content. `--credential` logs in with a [credential the manifest declares](modules.md#registry-logins) instead of the Docker configuration |

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

| Status | When |
|---|---|
| `0` | everything asked for succeeded. For `plan` without `--detailed-exitcode`, whether or not an apply would change anything; for `drift`, nothing drifted; for `import`, the documents were written, whatever was reported about them |
| `1` | an error: a document was invalid or did not compile; a tool failed; a policy denied a change or could not be asked; a stage could not be rolled back; the run was refused because the state is locked or there was nothing to resume or roll back; a [sink](reporting.md#sinks) marked `required` failed; a run was stopped by `--stage-timeout`; `fmt --check` found a file that is not formatted; `lint` found an error, or a warning with `--strict`; `migrate --check` found a file that would change; `import` could not read the source, no plugin imports it, or, with `--strict`, something was changed or not translated; `module verify`, `signing verify`, `secret check` or `rules check` found something wrong |
| `2` | `drift`: a stage has drifted. `plan --detailed-exitcode`: an apply would do something. Any command: a mistake in the command line itself |
| `130` | the run was [stopped with Ctrl-C](../concepts/run-safety.md#stopping-a-run), as a shell reports a command ended that way |

With `--output json` the object on standard output says the same: `ok` is
false for `1` and `130`, and the `summary` of a run carries `drifted` and
`hasChanges` for the two meanings of `2`
([machine-readable output](cli-json.md#what-every-object-has)).

A reader that stops early, as in `meridian plugins | head`, ends the printing
and nothing else: the command still runs to its end and exits with the status
above, so `meridian module verify | head -1` still fails on a tampered module.
If standard output cannot be written for any other reason, such as a full
disk, the command says so on standard error and exits `1`.

## Plugins and their tools

A plugin is a separate program named `meridian-target-<name>`. Each one runs its
tool as you would by hand, with the credentials and configuration the tool
already has. Every plugin reads a few environment variables that name a
different binary or add arguments to every call; they are listed on each
[target's page](../targets/ansible.md#running).
