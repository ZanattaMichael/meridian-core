# Machine-readable output: `--output json`

With `--output json` (or `MERIDIAN_OUTPUT=json`), a command prints **one JSON object** on
stdout at the end; the text report moves to stderr.

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

Each has a JSON Schema (draft 7) in [`schemas/output/`](../../schemas/output/), checked by
tests. Fields are added, never removed or renamed within a language version; ignore unknown
ones.

## What every object has

| Field | Meaning |
|---|---|
| `command` | the subcommand as it is typed: `plan`, `module verify`, `secret check` |
| `ok` | whether the command did what was asked. `false` means it exited `1`, or `130` when it was interrupted. A `plan` that found changes and a `drift` that found drift are `ok`: the check did its job, and what it found is in the object; the [exit status](cli.md#exit-status) says the same |

The rest is the command's own. Text that the records keep as "none" is
`null` in the objects, never `""`.

### When the command fails before it has a result

A command that stops early prints the error as an object:

```json
{
  "command": "plan",
  "ok": false,
  "error": {
    "message": "no document's labels match the selector tier=none"
  }
}
```

Also on stderr as `error: ...`. Schema: `schemas/output/error.schema.json`.

Invalid documents give the [`validate`](#validate) object under the command's name.
Command-line mistakes are text, exit `2`.

### What does not change

- `--events json` (one object per line) cannot be combined with `--output json`; use
  `--report json=FILE`.
- `meridian report --format json` writes the [report](reporting.md#json), a different shape.
- `lint --json` and `schema status --json` stay line-per-item.
- `meridian schema`, `import` without `--out`, and `lsp` are unaffected.

## The commands

### `compile`, `plan`, `apply`, `drift`, `rollback`

One run object. Schema: `schemas/output/run.schema.json`.

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
`asked`, `deny` and `warn`) and `previousGood` (the artifact hash a rollback
goes back to, or `null`).

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

A `change` is `resource`, `action` (`create`, `update`, `delete`,
`replace` or `adopt`) and `summary`, which may be empty.

**A rollback** has `name`, `kind`, `target`, `host` and a `status`:
`rolled-back` (with `to`, the artifact hash, and `changes`), `unchanged`,
`unavailable` or `failed` (with `error`), or `not-attempted` (with
`becauseOf`).

### `plan --detailed-exitcode`

Exit `2` for changes, `0` in sync, `1` on error. A stage has changes if the tool listed any,
cannot preview (`unknown`), or is `deferred`; `summary.hasChanges` uses the same test.

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

`documents` is the count when all validate, else `0`.

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

`ok` is false on an error, or a warning with `--strict`. `pluginsFound` false means
language-only checks.

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

### `retarget`

`file`, the file read; `out`, the file written; `outFrom`, what chose it (`cli`, `manifest` or `default`); `to`; `adoption`, how the new
target takes what exists (`import`, `converge` or `none`); one row per
document with `name`, `from`, `adopted` (the resource ids given `adopt:`)
and `notes`; and `warning`, set when the new target will create every
resource again. Schema: `schemas/output/retarget.schema.json`.

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
`failed` and `errors`. `ok` is true only when every case passed. Schema: `schemas/output/test.schema.json`.

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

A pull request job that fails only on error:

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

See also [reports in CI](reporting.md#in-ci).
