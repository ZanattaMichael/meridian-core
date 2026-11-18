# Machine-readable output: `--output json`

Every `meridian` command prints text for a person. With `--output json`, or
`MERIDIAN_OUTPUT=json` in the environment, a command instead prints **one JSON
object** on standard output, at its end, and the report for people moves to
standard error, as it does with `--events json`. A pipeline or a tool that
wraps Meridian reads the object; the wording of the text can then change
without breaking anything.

```
$ meridian plan site.yaml --output json --detailed-exitcode > plan.json; echo $?
network (Infrastructure, target fake)
  + create files/vm: 8 bytes
  2 changes to make
web (ResourceSet, target fake)
  deferred: needs the output "address" of Infrastructure "network", which has not been applied yet
2
$ jq .summary plan.json
{
  "stages": 2,
  "failed": 0,
  "changes": 2,
  "hasChanges": true,
  "drifted": 0,
  "succeeded": true,
  "interrupted": false
}
```

The objects are described here, one per command, and each has a JSON Schema
(draft 7) under [`schemas/output/`](../../schemas/output/) in the repository.
A test runs every command and checks its output against its schema, so the
schemas are the contract: a field is added only with its schema, and a field
is never removed or renamed within a language version. Added fields are the
one change to expect, so a reader should ignore fields it does not know.

## What every object has

| Field | Meaning |
|---|---|
| `command` | the subcommand as it is typed: `plan`, `module verify`, `secret check` |
| `ok` | whether the command did what was asked. `false` means it exited `1`, or `130` when it was interrupted. A `plan` that found changes and a `drift` that found drift are `ok`: the check did its job, and what it found is in the object; the [exit status](cli.md#exit-status) says the same |

The rest is the command's own. Text that the records keep as "none" is
`null` in the objects, never `""`.

### When the command fails before it has a result

A command that stops before it could do its work, because a file is missing,
a selector matches nothing, or the command line asks for something it cannot
have, prints the error as an object too, so that a reader never has to fall
back to standard error:

```json
{
  "command": "plan",
  "ok": false,
  "error": {
    "message": "no document's labels match the selector tier=none"
  }
}
```

The same message is on standard error, with `error:` in front, as always.
Schema: `schemas/output/error.schema.json`.

A command that reads documents, and finds one that does not read or does not
validate, prints the [`validate`](#validate) object instead, under its own
`command`, with the diagnostics. A `plan site.yaml` of a broken document is
therefore read exactly like a `validate site.yaml` of it.

A mistake in the command line itself (an unknown option, a missing argument)
is reported by the argument parser as text, and exits `2`, before any output
format is settled.

### What does not change

- `--events json` and `meridian events` write one JSON object per line, one
  per event, as they always did; `--output json` cannot be combined with
  `--events json`, since both want standard output. Use `--report json=FILE`
  for the run as a file beside the result.
- `meridian report --format json` writes the [report](reporting.md#json),
  which is a different shape from the run object below: the report is a fold
  over the events of a recorded run, and the run object is what the command
  that just ran saw.
- `meridian lint --json` and `meridian schema status --json` keep their line
  per finding or target. `--output json` on `lint` gives the whole result as
  one object instead, below.
- `meridian schema` prints a JSON Schema, `meridian import` without `--out`
  prints documents, and `meridian lsp` speaks to an editor. `--output json`
  does not change them.

## The commands

### `compile`, `plan`, `apply`, `drift`, `rollback`

One object, the run, for the five commands that run a workflow. Schema:
`schemas/output/run.schema.json`.

```json
{
  "command": "apply",
  "ok": true,
  "mode": "apply",
  "run": "2026-11-18T09-14-03Z-0cc7",
  "refused": null,
  "plan": null,
  "stages": [
    {
      "name": "network",
      "kind": "Infrastructure",
      "target": "terraform",
      "host": null,
      "credential": null,
      "status": "applied",
      "reason": null,
      "artifact": { "hash": "sha256:9f2c…", "files": ["main.tf.json"] },
      "warnings": [],
      "removed": [],
      "policy": { "allowed": true, "asked": true, "deny": [], "warn": [] },
      "changes": [
        { "resource": "aws_instance.web", "action": "create", "summary": "t3.small in eu-west-1" }
      ],
      "outputs": [
        { "name": "address", "value": "203.0.113.10", "sensitive": false },
        { "name": "admin_password", "value": "(sensitive)", "sensitive": true }
      ],
      "previousGood": null
    },
    {
      "name": "web",
      "kind": "ResourceSet",
      "target": "ansible",
      "host": "203.0.113.10",
      "credential": "deploy (ssh, user deploy)",
      "status": "applied",
      "reason": null,
      "artifact": { "hash": "sha256:51aa…", "files": ["inventory.ini", "playbook.yml"] },
      "warnings": [],
      "removed": [],
      "policy": null,
      "changes": [ { "resource": "nginx_conf", "action": "update", "summary": "" } ],
      "outputs": [],
      "previousGood": "sha256:40b1…"
    }
  ],
  "rollbacks": [],
  "warnings": [],
  "summary": {
    "stages": 2, "failed": 0, "changes": 2, "hasChanges": false,
    "drifted": 0, "succeeded": true, "interrupted": false
  }
}
```

| Field | Meaning |
|---|---|
| `mode` | `compile`, `plan`, `apply`, `drift` or `rollback` |
| `run` | the run's id, under which `plan`, `apply`, `drift` and `rollback` keep their [record](state.md). A `compile` has an id too, and records nothing |
| `refused` | `null`, or why the run did not start at all: `kind` is `locked` (with the `lock`, as [`status`](#status) shows it), `state`, `resume`, `rollback` or `plan` (an `apply --plan` whose saved plan names other stages, or a stage applied since it was taken), and `message` is the text. No stage was attempted |
| `plan` | `null`, or the [saved plan](../operations/pull-requests.md) the command wrote or applied: `file`, its path; `run`, the id of the run that took it (on an `apply --plan`; `null` on the `plan` that wrote it); `hasChanges`, whether an apply of it would do something |
| `stages` | one object per stage, in the order they ran; below |
| `rollbacks` | what an `apply --on-failure rollback` or a `rollback` put back, last stage first; below |
| `warnings` | things that went wrong beside the stages, such as the record not being written. They do not fail the run |
| `summary` | the counts: `stages`, `failed`, `changes` (every change the stages listed), `drifted` (stages that drifted), and the flags `hasChanges` (a plan: whether an apply would do something, which is what `--detailed-exitcode` exits `2` on), `succeeded` and `interrupted` |

**A stage.** `status` says what happened and which other fields are there.
The fields every stage has: `name`, `kind` (`Infrastructure` or
`ResourceSet`), `target`, `host` (the machine a ResourceSet configures, or
`null`), `credential` (which identity reaches it, in words, or `null`),
`reason` (for a failed stage, its [error code](errors.md); `null` otherwise),
`artifact` (what the document compiled to: its `hash` and the names of its
`files`, never their content; `null` when it did not get that far),
`warnings` (each with `target`, `resource` and `message`), `removed` (what
[conditions](dsl/conditions.md) left out: `subject` and `condition`),
`policy` (`null` when no [policy](policy.md) was asked; otherwise `allowed`,
`asked`, `deny` and `warn`), `analyses` (one per [analyzer](analyzers.md)
named with `--analyze`, in order; empty otherwise) and `previousGood` (the
artifact hash a rollback goes back to, or `null`).

Each of `analyses` has `analyzer`, `asked` (false when it could not look,
with `error` saying why), `counts` (`critical`, `high`, `medium`, `low`,
`info` of the findings the baseline did not accept, and `accepted`),
`findings` (each with `rule`, `severity`, `message`, `file`, `line`,
`resource`, `document`, `documentLine`, `url` and `accepted`; a place that is
not known is `null`) and `measures` (each with `name`, `value` and `unit`).

| `status` | What it means | Its own fields |
|---|---|---|
| `compiled` | the document compiled; as far as `compile` goes, and as far as a plan goes on a target that only compiles | `writtenTo`, the directory, with `compile --out` |
| `planned` | the tool previewed the change | `changes`; `unknown`, true when the tool cannot preview and an empty `changes` must not be read as in sync |
| `applied` | the tool applied the change | `changes`; `outputs`, each with `name`, `value` and `sensitive`. A sensitive value is the mask `(sensitive)`, never the value |
| `deferred` | the stage needs an output that does not exist yet | `waitingOn` (`name` and `kind` of the stage), `waitingFor` (the output) |
| `skipped` | a resumed run found it already applied from the same artifact | `appliedIn`, the run |
| `drift` | what a drift check found | `drift`: `never-applied`, `in-sync`, `drifted`, `pending` or `unknown`; `changes`, the differences for `drifted` and `pending`; `lastApplyFailed` for `pending` |
| `left-out` | the document's own `spec.when` was false | `condition` |
| `failed` | the stage failed and the run stopped here | `error`, the text; `changes`, what the tool had already done before failing; `reason`, the code |
| `not-run` | an earlier stage failed | `becauseOf` (`name` and `kind`) |

A `change` is `resource`, `action` (`create`, `update`, `delete` or
`replace`) and `summary`, which may be empty.

**A rollback** has `name`, `kind`, `target`, `host` and a `status`:
`rolled-back` (with `to`, the artifact hash, and `changes`), `unchanged`,
`unavailable` or `failed` (with `error`), or `not-attempted` (with
`becauseOf`).

### `plan --detailed-exitcode`

A `plan` exits `0` whether or not an apply would change anything. With
`--detailed-exitcode` it exits `2` when an apply would do something, `0` when
every stage is in sync, and `1` on an error, which is what `drift` already does
and what Terraform users expect of the flag of the same name. A pipeline can
then ask "is there anything to apply?" without reading the plan, and decide
whether to ask for an approval.

A stage counts as having changes when its tool listed any, when its tool
cannot preview (`unknown`), or when it is `deferred`: it waits on an output
that only an apply produces, so there is an apply to do. The same test is the
`summary.hasChanges` flag of the object.

### `status`

The current or last run, as the state records it, and the lock. Schema:
`schemas/output/status.schema.json`.

```json
{
  "command": "status",
  "ok": true,
  "backend": "the state directory .meridian",
  "run": {
    "id": "2026-11-18T09-14-03Z-0cc7",
    "mode": "apply",
    "status": "failed",
    "command": "apply site.yaml",
    "user": "mike",
    "host": "build-07",
    "selector": null,
    "started": "2026-11-18T09:14:03.204Z",
    "finished": "2026-11-18T09:15:11.910Z",
    "resumes": null,
    "steps": [
      { "name": "network", "kind": "Infrastructure", "target": "terraform", "endpoint": null,
        "status": "succeeded", "reason": null, "artifact": "sha256:9f2c…", "changes": 4, "previous": null },
      { "name": "web", "kind": "ResourceSet", "target": "ansible", "endpoint": "203.0.113.10",
        "status": "failed", "reason": "tool-failed", "artifact": "sha256:51aa…", "changes": 2, "previous": "sha256:40b1…" }
    ]
  },
  "lock": null
}
```

`run` is `null` when nothing has been recorded. Its `status` is `running`,
`succeeded`, `failed`, or `interrupted` for a run that never said how it ended
and does not hold the lock. A step's `status` is `running`, `succeeded`,
`failed`, `skipped`, `deferred`, `not-run` or `rolled-back`, and `reason` is
its [error code](errors.md) when it failed. `lock` is `null` or the holder:
`id`, `user`, `host`, `pid`, `command` and `since`.

### `runs`

Every recorded run, oldest first, each as `status` shows it. Schema:
`schemas/output/runs.schema.json`.

```json
{ "command": "runs", "ok": true, "backend": "the state directory .meridian", "runs": [ … ] }
```

### `unlock`

The lock that was removed, as `status` showed it. Schema:
`schemas/output/unlock.schema.json`.

### `validate`

Whether the documents read and validate, and every problem found, each with
`file`, `line` and `column` (1-based; `0` when the problem is the whole
file), the `path` of the field (such as `spec.resources[0].dependsOn[0]`;
empty for the whole document) and the `message`. Schema:
`schemas/output/validate.schema.json`.

```json
{
  "command": "validate",
  "ok": false,
  "documents": 0,
  "diagnostics": [
    { "file": "site.yaml", "line": 12, "column": 7, "path": "spec.resources[1].dependsOn",
      "message": "dependency cycle among 2 resources: key -> lock -> key" }
  ]
}
```

`documents` is how many documents were read when they all validate, and `0`
when any did not: a bundle is valid as a whole or not at all.

### `lint`

Every finding, in the shape `lint --json` prints one per line, and the counts.
Schema: `schemas/output/lint.schema.json`.

```json
{
  "command": "lint",
  "ok": false,
  "diagnostics": [
    { "file": "stacks/web.yaml", "line": 16, "column": 9, "endLine": 16, "endColumn": 14,
      "severity": "error", "code": "invalid-param", "source": "ansible",
      "message": "param \"mode\" of a file must be text, not a number", "fixable": false }
  ],
  "summary": { "errors": 1, "warnings": 0, "notes": 0, "fixed": 0, "documents": 3, "files": 2, "pluginsFound": true }
}
```

`ok` is false when there is an error, or a warning with `--strict`.
`pluginsFound` is false when no plugin was found, in which case the documents
were checked against the language only.

### `fmt`

One row per file, with `status` `ok` (already formatted), `formatted`
(rewritten), `not-formatted` (with `--check`), `failed` (with the `message`)
or `skipped` (not a Meridian document), and the counts. `ok` is false when a
file failed, or when `--check` found one that is not formatted. Schema:
`schemas/output/fmt.schema.json`.

### `migrate`

`to`, the language version moved to; `check`; and one row per file with
`status` `current`, `migrated`, `would-migrate`, `refused`,
`needs-decisions` (with the `decisions`), `unreadable` or `unwritable` (with
the `message`). Schema: `schemas/output/migrate.schema.json`.

### `plugins`

The catalogue: one object per plugin with `name`, `category`, `can` (the
operations, as the text lists them), `resources`, `versions`, `imports` (each
format's `name`, `description` and `patterns`), `sdk`, `protocol`, `binary`,
`signature` and `settings`. Settings are always included; the text needs
`--settings`. Schema: `schemas/output/plugins.schema.json`.

A `signature`, here and wherever one is shown, is `state` (`not-checked`,
`verified`, `unsigned` or `invalid`), with `keyId` when verified and `reason`
when invalid.

```json
{
  "command": "plugins",
  "ok": true,
  "plugins": [
    {
      "name": "dsc", "category": "configuration-management",
      "can": ["compile", "plan", "apply", "import"],
      "resources": ["<Module>/<Resource>", "runner"], "versions": ["2", "3"],
      "imports": [ { "name": "ps1", "description": "PowerShell DSC configurations", "patterns": ["*.ps1"] } ],
      "sdk": "0.1.0", "protocol": 2,
      "binary": "/usr/local/bin/meridian-target-dsc",
      "signature": { "state": "verified", "keyId": "780001b5de1d7691" },
      "settings": [ { "name": "program", "variable": "MERIDIAN_DSC_PROGRAM", "default": "dsc", "description": "…" } ]
    }
  ]
}
```

### `version`

`meridian`, the release; `languages`, each with `name`, `status`
(`supported`, `deprecated` or `removed`) and `release` (the release it was
deprecated or removed in, or `null`); `pluginSdk`; `pluginProtocol`, the range
as text, such as `2` or `2 to 3`; and `lockfileFormat`. Schema:
`schemas/output/version.schema.json`.

### `module list`, `module verify`, `module install`, `module update`

Each module as the lock pins it: `name`, `version`, `source`, `resolved`,
`integrity` and `requires`. `module verify` adds `ok`, `problem` and
`signature` per module, and is `ok` only when every module is. `module
install` and `module update` list `changes`, each a module with `change`
(`added`, `updated`, `removed` or `unchanged`) and `from` (the version an
update moved from), plus `wroteLock` and `lockFile`. Schemas:
`schemas/output/module-list.schema.json`,
`schemas/output/module-verify.schema.json`,
`schemas/output/module-install.schema.json`.

### `module sign`, `module pack`, `module push`

What was signed (`name`, `version`, `keyId`, `integrity`, `wrote`), packed
(`name`, `version`, `file`, `size`, `integrity`, `mediaType`) or published
(`name`, `version`, `repository`, `tag`, `digest`, `integrity`,
`alreadyPublished`). Schemas: `schemas/output/module-sign.schema.json`,
`schemas/output/module-pack.schema.json`,
`schemas/output/module-push.schema.json`.

### `rules check`

`rulesFile` (`null` when there is none), `rules` (how many), `layers` (each
`path` and `group`, least specific first) and `violations`, each with `rule`,
`check`, `layer`, `group`, `key` and `message`, and never a value. `ok` is
false when there is a violation. Schema:
`schemas/output/rules-check.schema.json`.

```json
{
  "command": "rules check",
  "ok": false,
  "rulesFile": null,
  "rules": null,
  "layers": [],
  "violations": [
    { "rule": "meridian/protect-variables", "check": "protect", "layer": "node/web01.yaml", "group": "team",
      "key": "variables.Region", "message": "layer \"node/web01.yaml\" (team) changes \"variables.Region\", which layer \"common.yaml\" (organization) set" }
  ]
}
```

When the layers break the rules the hierarchy is not reported, only the
violations; a hierarchy that cannot be loaded at all is an
[error](#when-the-command-fails-before-it-has-a-result).

### `secret check`

One object per document that declares a secret or an ambient credential
(`name`, `file`, `secrets`, `credentials`), each secret with `name`, `from`
(the provider), `optional`, `state` (`ok`, `not-checked`, `absent-optional`,
`missing`, `refused` or `unavailable`, the
[states of the text](cli.md#secret-check) as words), `answered` (whether
`plan` or `apply` would get past it) and `detail`; each credential declared
`from: ambient` with `name`, `type`, `state` (`ok`, `not-checked`, `missing`,
`refused` or `unavailable`), `answered` and `detail`; then `declared` and
`unanswered` for the secrets, and `credentialsDeclared` and
`credentialsUnanswered` for the credentials. No value is read or shown.
Schema: `schemas/output/secret-check.schema.json`.

### `secret set`

`name`, `from` and `storedIn`, the place in words. Schema:
`schemas/output/secret-set.schema.json`.

### `signing keygen`, `signing sign`, `signing verify`

The key pair written (`keyId`, `secret`, `public`); the signatures written
(`keyId` and `signed`, each `binary` and its `signature` file); or each
binary checked (`binary`, `verified`, `signature`). Schemas:
`schemas/output/signing-keygen.schema.json`,
`schemas/output/signing-sign.schema.json`,
`schemas/output/signing-verify.schema.json`.

### `test`

One object per test document under `files` (`file`, `name`, `ok` and its
`cases`), each case with `name`, `run` (`compile` or `plan`), `status`
(`passed`, `failed`, or `error` when the case could not run), `failures`
(one line each, empty when it passed), `documents` (each `name`, `target` and
`outcome`: `compiled`, `refused`, `left-out`, `planned`, `deferred`, `failed`
or `not-run`) and `millis`; then a `summary` of `files`, `cases`, `passed`,
`failed` and `errors`. `ok` is true only when every case passed. The report
for people moves to standard error. Schema: `schemas/output/test.schema.json`.

```json
{
  "command": "test",
  "ok": false,
  "files": [
    {
      "file": "tests/site.test.yaml",
      "name": "site",
      "ok": false,
      "cases": [
        { "name": "dev compiles nginx", "run": "compile", "status": "passed", "failures": [],
          "documents": [{ "name": "web", "target": "ansible", "outcome": "compiled" }], "millis": 12 },
        { "name": "production says so", "run": "compile", "status": "failed",
          "failures": ["assert contains(stages.web.files['playbook.yml'], 'welcome to prod'): false"],
          "documents": [{ "name": "web", "target": "ansible", "outcome": "compiled" }], "millis": 9 }
      ]
    }
  ],
  "summary": { "files": 1, "cases": 2, "passed": 1, "failed": 1, "errors": 0 }
}
```

## In a pipeline

A pull request job that plans, comments the changes, and fails only on an
error:

```yaml
- name: Plan
  id: plan
  run: |
    set +e
    meridian plan site.yaml --output json --detailed-exitcode > plan.json
    code=$?
    echo "code=$code" >> "$GITHUB_OUTPUT"
    [ "$code" -ne 1 ]
- name: Say what would change
  if: steps.plan.outputs.code == '2'
  run: |
    jq -r '.stages[] | select(.status == "planned") | "\(.name): \(.changes | length) changes"' plan.json
```

The report for people is on the job's log, since it went to standard error;
`plan.json` holds the plan as data; and the exit status says whether there is
anything to apply. [Reports and sinks](reporting.md#in-ci) shows the same job
keeping a JUnit file and an HTML page.
