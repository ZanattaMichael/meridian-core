# The `meridian` command

```
meridian validate <files>...
meridian compile  <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--out DIR]
meridian plan     <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--strict-secrets] [--detailed-exitcode] [--destroy]
meridian apply    <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--strict-secrets] [--resume | --resume=RUN] [--on-failure stop|rollback] [--stage-timeout DURATION]
meridian drift    <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE]
meridian rollback <files>... [--run RUN]
meridian destroy  <files>... [-l SELECTOR] [--data FILE] [--set KEY=VALUE] [--yes]
meridian status
meridian runs
meridian events   [RUN] [--type TYPE]
meridian logs     [RUN] [--step NAME] [--endpoint ENDPOINT] [--stream STREAM] [--grep RE] [--tail N] [--all]
meridian bug-report [RUN] [--out FILE]
meridian report   [RUN] [--format FORMAT] [--out FILE] [--from FILE]
meridian unlock   <ID>
meridian plugins
meridian import   <paths>... [--from TARGET] [--name NAME] [-o DIR] [-r] [--force] [--strict]
meridian migrate  <files>... [--to APIVERSION] [--check]
meridian retarget <file> --to TARGET [--out FILE] [--no-adopt] [--state-dir DIR]
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
meridian mcp      [--allow-plan] [--root DIR] [--state-dir DIR]
```

Every command takes `--output text|json` ([objects](cli-json.md)).

The files given to one command are one workflow (`---` separates documents).
Infrastructure runs before ResourceSets ([why](../concepts/two-stage-architecture.md)), and a
document runs after those it [imports](dsl/outputs-and-hand-offs.md#imports) from. A pinned
[module](modules.md) file is written `@<module>:<path>`:

```
meridian plan site.yaml @acme/nginx:documents/proxy.yaml --data @acme/nginx:data/defaults.yaml
```

The module must already be installed; commands never fetch and always verify the hash.

## Commands

### `validate`

Checks documents without plugins: required fields, unique ids, dependencies and cycles,
conditions and hand-offs. Errors give file, line and field:

```
site.yaml:12:7: spec.resources[1].dependsOn: dependency cycle among 2 resources: key -> lock -> key
```

Target checks happen in `compile` ([which check happens when](dsl/index.md#which-check-happens-when)).

### `compile`

Compiles each document to its target's files, changing nothing. `--out DIR` writes
`DIR/<document name>/`. A ResourceSet whose host output does not exist yet is deferred.

Before the plugin, Meridian checks the [kind pairing](dsl/index.md#the-document) and
[`targetVersion`](dsl/index.md#targetversion), and decides every
[`when`](dsl/index.md#when-and-runtimewhen) from `--data`. `plan` and `apply` compile first.

With `--analyze`, each document's compiled files go through the named
[analyzers](analyzers.md). A finding at or above `--fail-on` fails that document and the
command exits `1`, but the rest still compile.

### `plan`

Compiles and asks each tool what an apply would change, listed by document ids:

```
  + create ...
  ~ update ...
  - delete ...
  ! replace ...
  = adopt ...
```

`adopt` counts as a change ([adopting what exists](dsl/resources.md#adopting-what-exists)).
A tool that cannot preview reports `unknown`. A plan takes the
[state lock](../concepts/run-safety.md#one-run-at-a-time) and is recorded.

`--detailed-exitcode`: `2` for changes (including `unknown` and deferred), `0` in sync, `1`
on error ([run object](cli-json.md#compile-plan-apply-drift-rollback-destroy)).

`--policy` checks every stage ([policy](policy.md)); denials fail the stage and exit `1`,
but planning continues.

`--out FILE` (`-o`) [saves the plan](../operations/pull-requests.md) for `apply --plan`. Not
written if any stage failed or was denied.

`--destroy` previews [`destroy`](#destroy) instead: last stage first, every change a
`- delete`. Not with `--out` or `--policy`.

### `apply`

Compiles and applies each document in order, stopping at the first failure. Outputs are
shown (sensitive ones masked) and handed on. Holds the
[state lock](../concepts/run-safety.md#one-run-at-a-time) and is
[recorded](../concepts/run-safety.md#the-record). On failure:

```
web (ResourceSet, target ansible on 203.0.113.10)
  ~ update nginx_conf
  FAILED: the play failed (ran `ansible-playbook ...`, exit status 2)
run 2026-11-17T09-14-03Z-0cc7 failed. Once the cause is fixed, `meridian apply --resume site.yaml` continues from the stage that failed
```

`--resume` continues the last failed or interrupted apply; `--resume=<run>` a named one
(always with `=`). Stages already applied with unchanged artifacts are skipped
([rules](../concepts/run-safety.md#picking-up-where-a-run-stopped)).

`--plan FILE` applies only a saved plan; see
[what `apply --plan` checks](../operations/pull-requests.md#what-apply---plan-checks). Not with
`--resume`.

`--policy` checks every stage first; any denial applies nothing:

```
network (Infrastructure, target terraform)
  FAILED: the policy does not allow this change:
      - network: aws_db_instance.main would be a replace in prod
web (ResourceSet, target ansible)
  not run, because Infrastructure "network" failed
run 2026-11-17T09-14-03Z-0cc7 was stopped by its policy before anything was applied
```

`--on-failure rollback` [puts back](../concepts/run-safety.md#putting-a-run-back) every changed
stage, newest first. Still exits `1`.

Ctrl-C [stops the run](../concepts/run-safety.md#stopping-a-run) (`interrupted`, exit `130`;
twice exits at once). `--stage-timeout 30m` records `timed-out`. Both also apply to `plan`,
`drift` and `rollback`.

### `drift`

Checks what was applied is still in place. Each stage is `in sync`, `DRIFTED`, `pending`
(document changed since apply), `unknown` or `never applied`
([finding drift](../concepts/run-safety.md#finding-drift)).

```
web (ResourceSet, target ansible on 203.0.113.10)
  ~ update sshd_config
  DRIFTED: 1 difference from what was applied
drift: 1 of 2 stages drifted
```

Exit `2` on drift, `0` clean, `1` on error.

### `rollback`

Re-applies each stage of the last apply (or `--run RUN`) from its previous good artifact,
last first. Pass the same files. A stage that cannot be put back is `NOT ROLLED BACK`, exit
`1` ([cases](../concepts/run-safety.md#putting-a-run-back)).

### `destroy`

Removes what each stage's tool made, last stage first, and forgets the stage
([destroying](../operations/destroying.md)). Asks `Type yes to destroy:` first; `--yes`
(`-y`) skips the question and is required without a terminal or with `--output json`.

```
web (ResourceSet, target fake on 10.0.0.5)
  - delete files/motd
  destroyed 1 resource
network (Infrastructure, target terraform)
  - delete vm: aws_instance
  destroyed 1 resource
```

A stage is `kept` when its target cannot destroy or it was never applied. Stops at the first
failure; the failed stage stays recorded as applied, so `destroy` can be run again.

### `status`

The current or last run, its steps, and the lock.

```
run 2026-11-17T09-14-03Z-0cc7: apply, failed
  command:  meridian apply site.yaml
  started:  2026-11-17T09:14:03.204Z by mike@build-07
  finished: 2026-11-17T09:15:11.910Z
  network (Infrastructure, target terraform): succeeded, 4 changes
  web (ResourceSet, target ansible on 203.0.113.10): failed (tool-failed), 2 changes
not locked
```

### `runs`

Every recorded run, oldest first.

```
2026-11-17T09-14-03Z-0cc7  apply    failed     2 steps, 1 failed
2026-11-17T09-21-40Z-1f42  apply    succeeded  2 steps, resumes 2026-11-17T09-14-03Z-0cc7
```

### `events`

A run's [events](state.md#events) (default: the last), one per line. `--type` filters
(repeatable, `'step.*'` for a family).

```
meridian events --type step.failed --type 'resource.*'
```

### `logs`

Prints the tool output a run kept on this machine
([`runs/<id>/logs/`](state.md#runsidlogs)), the last run's when no run is
named. With no filter it shows the failed steps, or every step when none
failed; `--all` shows every step regardless.

```
$ meridian logs
== web (ResourceSet, target ansible on 203.0.113.10): failed (tool-failed)
   .meridian/runs/2026-11-17T09-14-03Z-0cc7/logs/resourceset/web@203.0.113.10.log
2026-11-17T09:15:09.880Z stdout TASK [nginx : install] ****
2026-11-17T09:15:11.102Z stderr fatal: [203.0.113.10]: FAILED! => {"msg": "No package matching 'nginx'"}
2026-11-17T09:15:11.204Z meridian step failed (tool-failed): ansible-playbook exited 2
```

| Option | Keeps |
|---|---|
| `--step NAME` | the step of that document |
| `--endpoint ENDPOINT` | the step on that machine, as `status` shows it |
| `--stream STREAM` | lines of one stream: `stdout`, `stderr`, or `meridian` (what Meridian added) |
| `--grep RE` | lines whose text matches the regular expression |
| `--tail N` | the last `N` lines of each step, after the other filters |

A step whose output was not kept (`--no-logs`, another machine, or a step
that ran no tool) is listed with `no tool output was kept on this machine`.

### `bug-report`

Writes one `.tar.gz` of a run, the last one when no run is named, for a bug
report. It sends nothing anywhere. `--out FILE` names the archive; the default
is `meridian-bug-report-<RUN>.tar.gz` in the current directory, readable only
by its owner.

| File in `meridian-bug-report-<RUN>/` | Holds |
|---|---|
| `run.json`, `events.jsonl` | the run's [record](state.md#runsidrunjson) and [events](state.md#events) |
| `logs/<kind>/<document>[@<endpoint>].log` | the tool output this machine kept, masked |
| `version.txt` | what `meridian version` prints |
| `plugins.json` | the plugins of the run's targets, as `meridian plugins --output json` lists them |

Everything in it is copied as it was kept, so it holds no secret value. The
record names the operating system user and the machine that ran; read the
archive before you share it.

### `report`

A [report](reporting.md) of a run: `--format json|junit|markdown|html`, `--out FILE`,
`--from FILE` (or `-`) for an events file.

```
meridian report --format junit --out meridian.xml
meridian apply site.yaml --events json | meridian report --from - --format html --out run.html
```

### `unlock`

Removes a stale lock by its id (from `status`). Make sure the run is really gone.

### `plugins`

Lists plugins: category, capabilities, resource types, tool versions, `spec.settings` keys,
[refusal rules](refusal-rules.md), binary and signature.

```
dsc
  category:  configuration-management
  can:       compile, plan, apply, import
  resources: <Module>/<Resource>, runner
  versions:  2, 3
  settings:  credential, engine, target
  debug:     1: $VerbosePreference = 'Continue', 2: $VerbosePreference = 'Continue' $DebugPreference = 'Continue'
  imports:   PowerShell DSC configurations (*.ps1)
  imports:   DSC v3 configuration documents and DscPipelineRunner projects (*.dsc.yaml, *.dsc.yml, *.dsc.json, *.dsc.config.yaml, *.dsc.config.yml, *.dsc.config.json, Datum.yml, *.yaml, *.yml, *.json)
  sdk:       0.1.0 (protocol 2)
  binary:    /usr/local/bin/meridian-target-dsc
  signature: verified, signed by key 780001b5de1d7691
```

`inputs` under `can` means it accepts [`spec.imports`](dsl/outputs-and-hand-offs.md#imports);
`import` means it has an importer. Settings keys are what
[`spec.settings`](dsl/index.md#settings) accepts.

`--settings` adds runner settings (name, variable, default, purpose). `--rules` adds
[refusal rules](refusal-rules.md).

```
ansible
  ...
  settings:
    program (MERIDIAN_ANSIBLE_PLAYBOOK, default "ansible-playbook")
      The `ansible-playbook` program to run, when it is not on `PATH`.
    args (MERIDIAN_ANSIBLE_ARGS, default none)
      Extra arguments for every run, split like a shell command line: ...
```

See the [plugin settings reference](plugin-settings.md). A `meridian.yaml` in the current
directory supplies `spec.settings`; undeclared names stop the plugin.

### `import`

Turns a tool's configuration into documents, running no tool.

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

The stderr report lists documents, then what was **not translated**, **changed**, and
**notes**; the first two are also `TODO(meridian-import)` comments. Documents are validated
before writing. See [importing](../operations/importing.md).

`sdk` and `protocol` (for `plugins`) are the SDK and negotiated protocol. An incompatible
plugin fails the command ([compatibility](compatibility.md)).

### `migrate`

Rewrites documents to a newer `apiVersion`, in place.

```
meridian migrate site.yaml modules/*.yaml
meridian migrate site.yaml --to meridian/v1
meridian migrate site.yaml --check
```

- `--to` (default: newest).
- `--check` exits `1` if any file would change.

One line per file:

```
site.yaml: already at meridian/v1
```

Comments and layout are kept; only top-level `apiVersion` changes; files are rewritten
whole or not at all. Decisions it cannot make are listed and the file is left (exit `1`).
Never downgrades. `meridian/v1` is the only version today.

### `retarget`

Moves a document to another target. It writes a new file with `spec.target`
changed and, when the new tool imports, an `adopt:` line under each resource
the old tool manages, holding the identifier the old tool reports.

```
meridian retarget site.yaml --to opentofu
meridian retarget site.yaml --to opentofu --out next/site.yaml
meridian retarget web.yaml --to salt --no-adopt
```

- `--out` names the file to write. Without it, `spec.retarget.out` in the
  project's `meridian.yaml` decides, and without that `site.opentofu.yaml`
  beside `site.yaml`. An existing file is refused.
- `--no-adopt` reads nothing from the old tool and writes no `adopt:`.
- Without `--no-adopt`, a document the old tool has not applied, or an old
  target whose plugin cannot report what it manages, is refused.

The file read is never changed, and no tool changes anything: the old tool's
state is only read. [Moving a document to another tool](../operations/retargeting.md)
has the whole procedure and its limits.

### `version`

Shows the core, language, plugin SDK and protocol versions.

```
meridian 0.1.0
  language         meridian/v1  supported
  plugin sdk       0.1.0
  plugin protocol  2
  lockfile format  1
```

Languages are `supported`, `deprecated since …` or `removed in …`; `lockfile format` is that of
[`meridian.lock`](modules.md#the-lockfile). `--version` prints the first line
([compatibility](compatibility.md)).

### `rules check`

Checks a hierarchy's layers against its [compile rules](compile-rules.md), loading no
plugin.

```
$ meridian rules check --hierarchy config/hierarchy.yaml --set env=prod --set node=web01
error: compile rules: 1 violation; nothing was compiled
  - [meridian/protect-variables] layer "node/web01.yaml" (team) changes "variables.Region", which layer "common.yaml" (organization) set
```

Rules come from `rules.yaml` beside the hierarchy, or `--rules`. `--require-rules` makes a
missing file an error. `--data` and `--set` are checked as `(command line)` layers
([details](compile-rules.md#running-the-check)).

### `secret check`

For each declared [secret](dsl/secrets.md), says where it comes from and whether it would be
found, without reading it.

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

With [several places](dsl/secrets.md#several-places-for-one-secret), the first that answers is
shown. Secret Service and keychain are `not checked`. [`from: ambient`](dsl/credentials.md#from-ambient)
credentials are checked too:

```
$ meridian secret check site.yaml
site (site.yaml)
  db_password  env    ok: the environment variable SITE_DB_PASSWORD is set
  deployer     aws    ambient, ok: the environment holds AWS_WEB_IDENTITY_TOKEN_FILE and AWS_ROLE_ARN (web identity, such as a pipeline's OIDC token)
```

Clouds: environment, login files, then the identity endpoint (1 s timeout). `ssh`:
`SSH_AUTH_SOCK`. `kubeconfig`: `KUBECONFIG`. Others are `not checked`. `MISSING` or `REFUSED`
fails ([details](../operations/running-in-a-pipeline.md#checking-the-jobs-identity-first)).

### `secret set`

Stores [secret](dsl/secrets.md#storing-a-secret) `NAME` in its first writable place
(`keyring` or `secret-service`), read from stdin or a hidden prompt.

```
$ meridian secret set deploy_token site.yaml
Value for deploy_token (not shown):
stored deploy_token in the kernel keyring, key meridian:deploy-token
$ printf '%s' "$TOKEN" | meridian secret set deploy_token site.yaml
stored deploy_token in the kernel keyring, key meridian:deploy-token
```

A trailing newline is dropped; an empty value stores nothing. Undeclared or unwritable
secrets are refused.

### `test`

Runs [`Test` documents](testing.md). Nothing is applied or recorded.

```
$ meridian test
tests/site.test.yaml (site)
  ok    dev compiles nginx and the message (compile, 1 document)
  FAIL  production says so (compile, 1 document)
        - assert contains(stages.web.files['playbook.yml'], 'welcome to prod'): false
  ok    a site with no web tier compiles nothing (compile, 1 document)
3 cases: 2 passed, 1 failed
```

Defaults to `tests/*.test.yaml`. `--case TEXT` filters; `--junit FILE` writes JUnit. Exit `1`
if any case fails or nothing ran.

### `fmt`

Rewrites documents in the standard layout, never changing meaning. `--check` exits `1` if
any need it; `--keep-order` keeps field order; `-` uses stdin/stdout. See
[the formatter](../tooling/formatter.md).

### `lint`

Reports what `validate` and `compile` would refuse, plus likely mistakes. Nothing is written
or run.

```console
$ meridian lint stacks/
stacks/web.yaml:16:9: error: param "mode" of a file must be text, not a number; write it quoted, so it is not read as a number: "420" [invalid-param]
stacks/web.yaml:16:15: warning: mode 0644 is read as a number, not as permissions; write it as a quoted string [unquoted-mode]
stacks/web.yaml:35:21: error: spec.resources[3].dependsOn[0].resource: resource "warm" depends on unknown resource "wbe" [invalid-document]
2 errors, 1 warning, 0 notes
```

`--fix`, `--strict`, `--json`, `--data`, `--set` (without data, every `when` is true).
`--state-dir` supplies modules and [tool schemas](../tooling/schema-sync.md). See
[the linter](../tooling/linter.md) and [lint rules](lint.md).

### `schema`

Prints a JSON Schema (draft 7) of the language, plugin types and cached tool schemas.
`--target` for one target, `-o` for a file. See [the JSON Schema](../tooling/json-schema.md).

### `schema sync`, `schema status`, `schema clean`

`schema sync` caches tool schemas for used types in `.meridian/schemas/<target>.json` and
records them in `meridian.lock` (`--target`, `--check`, `--no-lock`). `--locked` uses only
what the lock pins; `--all-types` describes every type (for packing a bundle);
`--prefer bundles|tool` chooses between `spec.schemas` bundles (default) and tools;
`--schema-signatures off|warn|require` sets bundle signature strictness.

`schema update` re-chooses the newest bundles for the documents and moves the lock.
`schema status` shows the cache against the lock (`--json`). `schema clean` removes caches.
See [tool schemas](../tooling/schema-sync.md).

### `schema pack`, `schema sign`, `schema push`, `schema fetch`

Make and move [schema bundles](../tooling/schema-bundles.md): one target's
tool schemas at one set of pins, signed and published to an OCI registry so
that other machines need no tool.

```
meridian schema pack terraform --name hashicorp-aws --version 5.60.0 [--prefix aws_]... [--key FILE] [--out FILE]
meridian schema sign FILE --key FILE [--out FILE]
meridian schema push FILE ghcr.io/acme/meridian-schemas [--credential NAME] [--insecure] [--no-index]
meridian schema fetch oci://ghcr.io/acme/meridian-schemas/terraform/hashicorp-aws:5.60.0 [PATHS]... [--no-lock] [--credential NAME] [--insecure]
```

| Command | Does |
|---|---|
| `pack` | writes the target's cache as a bundle archive (default `.meridian/schemas/packed/<target>-<name>-<version>.tar.gz`); `--prefix` keeps only types whose names start with the text; `--key` signs it |
| `sign` | signs a packed bundle, writing it again with `meridian.sig` inside |
| `push` | publishes it to `<registry>/<target>/<name>:<version>` (or a `file://DIR` registry) and lists it in the registry's index; a version is published once |
| `fetch` | writes a target's cache from one bundle, by `oci://` reference or file, checks its signature by `--schema-signatures` or `spec.schemas.signatures`, and records it in the lock; a registry `spec.schemas` names uses its own login, another takes `--credential` and `--insecure` as `push` does |

### `lsp`

The language server on stdio. Takes the plugin options and `--trust-editor-plugin-dirs`.
See [the language server](../tooling/language-server.md).

### `mcp`

The MCP server on stdio. `--root DIR` confines files; `--allow-plan` offers `plan`. See
[the MCP server](../tooling/mcp-server.md).

### `signing`

Makes keys, signs and verifies plugin binaries ([guide](plugin-signatures.md)).

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
| `--lock-timeout DURATION` | | `0s` | `plan`, `apply`, `drift`, `rollback`, `destroy`: how long to wait for another run to release the lock, as in `30s`, `5m`, `1h`. By default a locked state is refused at once |
| `--resume`, `--resume=RUN` | | | `apply` only: continue the last apply, or the run named (joined with `=`) |
| `--on-failure ACTION` | | `stop` | `apply` only: `stop` leaves what was applied in place; `rollback` puts back every stage the run changed |
| `--stage-timeout DURATION` | | none | `plan`, `apply`, `drift`, `rollback`, `destroy`: stop a stage whose tool has run this long, as in `30m` or `2h`, and fail the run as `timed-out`. By default a tool may take as long as it takes |
| `--run RUN` | | the last apply | `rollback` only: the apply to put back |
| `--destroy` | | | `plan` only: preview `destroy` instead of `apply` |
| `-y`, `--yes` | | | `destroy` only: destroy without asking |
| `--policy PATH` | | no policy | `plan`, `apply`: a [policy](policy.md) file or directory every stage is checked against before anything is applied; may be repeated |
| `--policy-engine NAME` | `MERIDIAN_POLICY_ENGINE` | `opa` | `plan`, `apply`: the program `meridian-policy-<NAME>` that reads the policies |
| `--analyze NAME` | `MERIDIAN_ANALYZERS` | none | `compile`, `plan`, `apply`: an [analyzer](analyzers.md), the program `meridian-analyze-<NAME>`, every stage's compiled files are put through; may be repeated, or `checkov,trivy` |
| `--analyze-config NAME.KEY=VALUE` | | | a setting for one analyzer; may be repeated |
| `--fail-on SEVERITY` | `MERIDIAN_FAIL_ON` | `high` | the severity at which an analyzer's finding fails its stage: `critical`, `high`, `medium`, `low`, `info` or `none` |
| `--analyze-baseline FILE` | | none | a file of [accepted findings](analyzers.md#the-baseline) that fail nothing |
| `--events json` | | | `compile`, `plan`, `apply`, `drift`, `rollback`: write every [event](state.md#events) to standard output as it happens, one JSON object per line. The report for people moves to standard error |
| `--report FORMAT=FILE` | | | `compile`, `plan`, `apply`, `drift`, `rollback`: write a [report](reporting.md#--report) of the run when it ends, as `json`, `junit`, `markdown` or `html`; may be repeated |
| `--sink NAME` | `MERIDIAN_SINKS` (separated by `,`) | | the same commands: send every event, as it happens, to a [sink](reporting.md#sinks); may be repeated |
| `--sink-config SINK.KEY=VALUE` | | | a setting for a sink; may be repeated. `on` chooses the event types and `required=true` fails the run when the sink fails |
| `--sink-timeout DURATION` | | `30s` | how long a sink has to finish sending after the run's last event |
| `--keep-runs N` | | `50` | `plan`, `apply`, `drift`, `rollback`: how many runs' records to keep; a run's [tool logs](state.md#runsidlogs) go with its record |
| `--log-limit SIZE` | | `10MiB` | `plan`, `apply`, `drift`, `rollback`: the most of one step's tool output kept, as in `512KiB` or a number of bytes. Past it the start and the end are kept, with a line saying how much was cut |
| `--no-logs` | `MERIDIAN_NO_LOGS` | off | the same commands: keep no tool output in the state directory. It is still printed |
| `--tool-debug`, `--tool-debug=LEVEL` | `MERIDIAN_TOOL_DEBUG` | `0` | `plan`, `apply`, `drift`, `rollback`: ask each step's tool for its own debug output, level `1` to `3`; the bare flag is `1`. [Asking a tool for more output](#asking-a-tool-for-more-output) |
| `--tool-debug-step NAME` | | every step | with `--tool-debug`: only the document named; may be repeated |
| `--out DIR` | | | `compile` only: where to write the compiled files |
| `-l SELECTOR`, `--selector SELECTOR` | | every document | `compile`, `plan`, `apply`, `destroy`: run only the documents whose `metadata.labels` match, as in `tier=web,env!=prod`. May be repeated; every requirement must hold. [Labels and selectors](dsl/index.md#labels-and-selectors) |
| `--data FILE` | | no data | `compile`, `plan`, `apply`: a YAML or JSON mapping of values that [`when`](dsl/index.md#when-and-runtimewhen) conditions and [interpolations](dsl/interpolation.md) read as `data.<key>`. May be repeated; a later file's key replaces an earlier file's |
| `--strict-secrets` | `MERIDIAN_STRICT_SECRETS` | off | `plan`, `apply`: stop a stage whose secret cannot be [locked in memory](#secrets), instead of warning once and going on |
| `--set KEY=VALUE` | | | `compile`, `plan`, `apply`: one value for `when` conditions and interpolations, applied after every `--data` file, so it wins. `KEY` may be dotted to set one key inside a mapping (`region.name=eu-west-1`). The value is typed as YAML types it. May be repeated. See [giving the data](dsl/conditions.md#giving-the-data) |
| `--annotations MODE` | `MERIDIAN_ANNOTATIONS` | `auto` | any command: inside a [pipeline engine](#inside-a-pipeline-engine), also write each problem in the engine's annotation syntax and the run's report as the job's summary. `auto` does so when standard output is not a terminal, `on` does so regardless, `off` never does |

### Inside a pipeline engine

When `GITHUB_ACTIONS`, `TF_BUILD`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `ARGO_*` or
`TEKTON_*` is set and stdout is not a terminal, `meridian` also writes:

- problems as annotations on stderr (GitHub Actions and Azure Pipelines only);
- the Markdown report as the job summary (`$GITHUB_STEP_SUMMARY`,
  `##vso[task.uploadsummary]`, `buildkite-agent annotate`).

```
$ GITHUB_ACTIONS=true meridian validate site.yaml 2>&1 | cat
error: site.yaml:34:7: spec.resources[0]: unknown field "colour"
::error file=site.yaml,line=34,col=7,title=Meridian%3A validate::spec.resources[0]: unknown field "colour"
```

Stdout is untouched. `--annotations off|on` overrides detection (Buildkite needs `on`).
See [running in a pipeline](../operations/running-in-a-pipeline.md).

### Asking a tool for more output

```
meridian apply site.yaml --tool-debug=2 --tool-debug-step web
```

Each target maps the level to its tool's own switch: `-vvv` for Ansible,
`TF_LOG=DEBUG` for Terraform. The [capability matrix](capability-matrix.md#--tool-debug)
lists every target's levels, and `meridian plugins` prints them under `debug:`.

- A level above the target's last uses its last.
- A target with no levels runs as usual, with a warning.
- The output is printed and kept in the [step's log](#logs), masked like every tool line.
- The level is recorded in the run's `run.started` event and in its command.

### Running part of a workflow

```
meridian apply site.yaml -l tier=web
```

Only `tier: web` documents run; others are still checked and can supply hosts. Matching
nothing is an error.

### Deciding conditions

```
meridian plan site.yaml --data common.yaml --data prod.yaml
```

`when` is decided from the data; an unset key is an error.
[Interpolation](dsl/interpolation.md) (`${{ data.env }}`) reads the same values. Decisions are
printed per stage; a document left out by `spec.when` is skipped:

```
site (ResourceSet, target ansible on shop-dev.example.com)
  when: 2 removed
    resource "site_cert": cond.needs_tls is false
    "site_conf" depends on "site_cert": cond.needs_tls is false
  compiled 2 files into build/site
monitoring (Infrastructure, target kubernetes)
  skipped: its when (data.monitoring.enabled ?? false) is false
```

`-l` matches interpolated labels.

### Secrets

[Secrets](dsl/secrets.md) are fetched only by `plan` and `apply`, just before their stage,
and wiped after. `compile` writes `(secret db_password)`. Output shows `(sensitive)`. Core
dumps are off and memory is locked ([details](../concepts/secrets-in-memory.md)). If
`ulimit -l` is too small:

```
site (ResourceSet, target ansible on web01)
  warning: secrets: the secret "db_password" could not be locked in memory and may reach swap. Raise the limit on locked memory (ulimit -l, or LimitMEMLOCK= for a service). --strict-secrets makes this an error
```

`--strict-secrets` stops the stage instead. See also [`secret check`](#secret-check).

### `module`

Manages the [modules](modules.md) `meridian.yaml` requires.

| Command | Does |
|---|---|
| `meridian module install` | Resolves the requirements, fetches what is missing, writes `meridian.lock`. With `--locked`, fails if the lockfile would change |
| `meridian module update [NAME]...` | Moves the named modules, or all of them, to the newest version that still fits |
| `meridian module verify` | Checks every pinned module against its hash, and its signature when `--module-signatures` says to; exits `1` if any fails |
| `meridian module list` | Lists the pinned modules, their versions and sources |
| `meridian module sign --key FILE` | Signs the module in the current directory, writing `meridian.sig` in it |
| `meridian module pack [--out FILE]` | Writes the module in the current directory as the archive a registry stores |
| `meridian module push REPOSITORY [--insecure] [--credential NAME]` | Publishes the module in the current directory to an OCI registry under its version. A version that is already published is never replaced with other content. `--credential` logs in with a [credential the manifest declares](modules.md#registry-logins) instead of the Docker configuration |

Only `install`, `update` and `push` use the network ([commands](modules.md#commands)).

### A deprecated language version

One warning per run:

```
warning: 14 documents, the first being "web", use apiVersion "meridian/v1", which is deprecated since meridian 2.0.0 and will stop being read; run `meridian migrate --to meridian/v2` to move to meridian/v2
```

No version is deprecated today.

## Exit status

| Status | When |
|---|---|
| `0` | everything asked for succeeded. For `plan` without `--detailed-exitcode`, whether or not an apply would change anything; for `drift`, nothing drifted; for `import`, the documents were written, whatever was reported about them |
| `1` | an error: a document was invalid or did not compile; a tool failed; a policy denied a change or could not be asked; a stage could not be rolled back; the run was refused because the state is locked or there was nothing to resume or roll back; a [sink](reporting.md#sinks) marked `required` failed; a run was stopped by `--stage-timeout`; `fmt --check` found a file that is not formatted; `lint` found an error, or a warning with `--strict`; `migrate --check` found a file that would change; `retarget` could not read the old tool or write the new file; `import` could not read the source, no plugin imports it, or, with `--strict`, something was changed or not translated; `module verify`, `signing verify`, `secret check` or `rules check` found something wrong; `test` had a case that failed or could not run, or ran none |
| `2` | `drift`: a stage has drifted. `plan --detailed-exitcode`: an apply would do something. Any command: a mistake in the command line itself |
| `130` | the run was [stopped with Ctrl-C](../concepts/run-safety.md#stopping-a-run), as a shell reports a command ended that way |

In JSON, `ok` is false for `1` and `130`; `summary.drifted` and `summary.hasChanges` explain
`2` ([output](cli-json.md#what-every-object-has)). A closed pipe (`| head`) does not change the
exit status; other stdout write failures exit `1`.

## Plugins and their tools

A plugin is a program `meridian-target-<name>` that runs its tool with the tool's own
credentials. Environment variables for the binary and extra arguments are on each
[target's page](../targets/ansible.md#running).
