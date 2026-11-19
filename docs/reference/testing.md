# Testing documents and modules

`meridian test` runs `kind: Test` documents. Each case compiles, plans, or applies and destroys
the named documents with its data and checks outcomes, files, refusals, changes and outputs,
using fields and [expressions](dsl/expressions.md).

<p align="center"><img src="../graphics/meridian-test-flow.svg" alt="meridian test: a Test document names documents and cases; each case compiles, plans, or applies and destroys them with its data in a scratch directory and checks the outcome, the files, the refusal, the changes and the outputs" width="100%"></p>

It reports every failure (text, `--output json`, `--junit`) and finds `tests/*.test.yaml`
itself. Plan and lifecycle cases need the tool (or a stand-in via
[plugin settings](plugin-settings.md)); a lifecycle case also needs somewhere to apply to.

## A first test

A document and data file:

```yaml
# site.yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  when: data.web
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: nginx
      type: package
      params:
        name: nginx
    - id: motd
      type: file
      params:
        path: /etc/motd
        content: "${{ data.motd }}"
```

The test, under `tests/`:

```yaml
# tests/site.test.yaml
apiVersion: meridian/v1
kind: Test
metadata:
  name: site
spec:
  documents: [../site.yaml]
  set:
    web: true
    motd: "welcome to ${{ data.env }}"
    env: dev
  cases:
    - name: dev compiles nginx and the message
      expect:
        web:
          outcome: compiled
          files:
            playbook.yml:
              contains: ["name: nginx", "welcome to dev"]
      assert:
        - length(keys(stages.web.files)) == 1
    - name: production says so
      set: {env: prod}
      assert:
        - contains(stages.web.files['playbook.yml'], 'welcome to prod')
    - name: a site with no web tier compiles nothing
      set: {web: false}
      expect:
        web: {outcome: left-out}
```

```
$ meridian test
tests/site.test.yaml (site)
  ok    dev compiles nginx and the message (compile, 1 document)
  ok    production says so (compile, 1 document)
  ok    a site with no web tier compiles nothing (compile, 1 document)
3 cases: 3 passed
```

A failing case:

```
tests/site.test.yaml (site)
  FAIL  dev compiles nginx and the message (compile, 1 document)
        - document "web": file "playbook.yml" does not contain "welcome to dev"
        - assert length(keys(stages.web.files)) == 1: false
1 case: 1 failed
```

## The Test document

| Field | Required | Value |
|---|---|---|
| `apiVersion` | yes | `meridian/v1` |
| `kind` | yes | `Test` |
| `metadata.name` | yes | The test's name, shown in the report and used as the JUnit suite name |
| `metadata.description` | no | One line on what the test covers |
| `spec.documents` | yes | The documents under test: one path or a list, relative to the test file, or a [module file](modules.md#using-a-modules-files) written `@acme/nginx:documents/proxy.yaml`. Every case loads all of them, as one `meridian compile` of the same files would |
| `spec.data` | no | Data files read for every case, as `--data` reads them, relative to the test file |
| `spec.set` | no | A mapping merged over the data for every case. A mapping inside a mapping is merged key by key; anything else replaces what was there, as `--set a.b=1` does |
| `spec.select` | no | A [label selector](cli.md#running-part-of-a-workflow), as `-l` takes it, that every case runs under |
| `spec.cases` | yes | The cases, at least one, each with a name no other case has |

Lint, fmt and the language server skip `Test` documents; `compile` refuses them. With no
files, `meridian test` looks for `*.test.yaml`.

## Cases

Each entry of `spec.cases` is one run with its own data and checks.

| Field | Required | Value |
|---|---|---|
| `name` | yes | What the case checks, in a few words. Unique within the test |
| `run` | no | `compile` (the default), `plan` or `lifecycle`. See [levels](#levels) |
| `data` | no | Data files read after the test's `spec.data` |
| `set` | no | Values merged over the data after the test's `spec.set` |
| `select` | no | A label selector that replaces the test's `spec.select` for this case |
| `expect` | one of the two | What each document should have come to, by the document's `metadata.name`. See [expectations](#expectations) |
| `assert` | one of the two | Expressions that must each be true. See [assertions](#assertions) |
| `phases` | no | Lifecycle only. See [lifecycle cases](#lifecycle-cases) |
| `timeout` | no | Lifecycle only. How long provision and verify may take together: `90`, `30s`, `20m`, `1h` |

A compile or plan case that checks nothing is refused; a lifecycle case may check nothing
beyond provisioning and destroying. All fields are checked before any case runs.

### Levels

A **compile** case runs `meridian compile`; no tool runs.

A **plan** case also runs `meridian plan` in an emptied scratch directory under the state
directory's `tests/`; project state is untouched. Remote state (a Terraform backend) is
still used. Stages waiting on outputs are `deferred`. A `meridian.yaml` beside the test can
point [plugin settings](plugin-settings.md) at a stand-in.

A **lifecycle** case applies to a scratch directory, checks the result, then
[destroys](../operations/destroying.md) it. See [lifecycle cases](#lifecycle-cases).

## Lifecycle cases

```yaml
cases:
  - name: web tier
    run: lifecycle
    set: {replicas: 2}
    phases:
      provision: {}
      verify: {drift: none}
      destroy: {}
    timeout: 20m
    expect:
      web: {outcome: applied}
    assert:
      - stages.network.outputs.address != ''
```

| Phase | Does | Fails when |
|---|---|---|
| `provision` | `meridian apply` in the case's scratch directory | A document fails or does not run, or `timeout` passes |
| `verify` | Checks `expect` and `assert` against the apply. With `drift: none`, plans again | A check fails; with `drift: none`, the plan would change anything or cannot say |
| `destroy` | `meridian destroy` of the same directory, always, after any failure or timeout | A document's destroy fails. The scratch directory is then kept and named |

- `phases` may be left out; `provision` and `destroy` take no fields. `verify.drift` is `none`
  or `any` (the default).
- Verify is skipped when provision failed.
- `break` and `diagnose` are refused: they come with the fault catalogue
  ([issue 237](https://github.com/ZanattaMichael/meridian-core/issues/237)).
- Every target in the documents must [destroy](../operations/destroying.md#which-targets-destroy);
  otherwise the case is an error before anything is applied.
- The scratch directory is `tests/<test>/<n>-<case>` under the state directory. It is removed
  after a destroy that succeeds. If it exists when the case starts (kept, or a destroy failed),
  the case is an error: destroy what it holds with `meridian destroy --yes --state-dir DIR`
  and the same documents and data, then remove it.
- `--keep` skips destroy and leaves the environment and its directory, to look at.
- `timeout` stops the running tool; destroy then runs with the same limit per tool.

```
$ meridian test
tests/site.test.yaml (site)
  FAIL  web tier (lifecycle, 2 documents)
        provision: ok
        verify: FAIL
        destroy: ok
        - verify: assert stages.network.outputs.address != '': false
1 case: 1 failed
```

## Expectations

`expect` maps document names to what they should come to. Unknown names fail.

| Field | Checks |
|---|---|
| `outcome` | The document's outcome is this word: `compiled`, `refused`, `left-out` (its own `spec.when` was false), `planned`, `deferred` (a plan case waiting for an output), `applied` (a lifecycle case), or `failed` (the plugin or the tool failed) |
| `rule` | The document was refused, and the refusal carries this [rule](refusal-rules.md) |
| `resource` | The refusal names this resource |
| `message` | The refusal's message holds this text |
| `files` | What the files the document compiled to hold, by the path the plugin gives each. Each file takes `contains` (text, or a list of texts, each of which must appear), `notContains`, `equals` (the whole file, exactly) and `matches` (a [pattern](dsl/expressions.md#patterns), or a list; the same patterns as `regexMatch`) |
| `warnings` | Text, or a list of texts, each of which some warning of the compile holds |
| `changes` | In a plan case, changes that must be among the tool's: each names an `action` (`create`, `update`, `delete`, `replace`, `adopt`), a `resource` (the id, or the tool's own name for what changes, as `meridian plan` prints it), or both |
| `changeCount` | In a plan case, exactly how many changes the tool reports. A tool that cannot preview fails this |

Prefer triggering refusals through case data, as the
[example](../../examples/tests/tests/web.test.yaml) does, so `meridian lint` stays clean. Rule
names are the same on every target ([refusal rules](refusal-rules.md)).

## Assertions

`assert` is a list of expressions that must be true, as text or `{expr, message}`:

```yaml
assert:
  - stages.web.outcome == 'compiled'
  - expr: contains(stages.web.files['playbook.yml'], 'become: true')
    message: the playbook must escalate
  - all(map(stages.network.changes, 'c', c.action == 'create'))
```

Same language as `when`, checked on read. Names: `stages` ([below](#what-stages-holds)) and
`data` (the case's data). A non-boolean result or an unset path is an error (use
`defined()`).

## What `stages` holds

`stages.<name>`, for each document including ones left out:

| Field | Value |
|---|---|
| `name`, `kind`, `target` | The document's name, `Infrastructure` or `ResourceSet`, and its target |
| `outcome` | The word the table above lists, plus `not-run` for a document after one that failed |
| `files` | The emitted files by path, each as its text; `{}` when the document was not compiled |
| `warnings` | The compile's warnings, each as one line of text |
| `refusal` | `null`, or a mapping of `rule`, `resource` and `message` for a refused document. A refusal the orchestrator makes itself, such as a `ResourceSet` sent to an infrastructure target, carries no rule and an empty `resource` |
| `changes` | In a plan case, the tool's changes, each with `action`, `resource` and `summary`; `[]` otherwise |
| `unknown` | Whether the tool could not preview, so `changes` says nothing |
| `outputs` | In a lifecycle case, the document's outputs by name; a sensitive one reads `(sensitive)`. `{}` otherwise |
| `condition` | The `when` that left the document out, or `null` |
| `error` | The failure's text when the document failed or was refused, or `null` |

A document whose conditions read a secret is reported as refused, as `compile` does.

## Running tests

```
meridian test                          # every tests/*.test.yaml
meridian test tests/site.test.yaml     # these files
meridian test --case prod              # only the cases whose name holds "prod"
meridian test --junit report.xml       # also write JUnit XML
meridian test --output json            # one object, for a machine
```

Every case runs. Exit `0` only if all pass; `1` otherwise, including unreadable files, no
files, or no `--case` match.

Each case prints `ok`, `FAIL` or `ERR` (could not run), its name, level and document count,
then a lifecycle case's phases, then one line per failure. `--output json` prints
[one object](cli-json.md#test); `--junit FILE` writes one `testsuite` per file and one
`testcase` per case, or per phase of a lifecycle case (`web tier: verify`), a skipped phase as
`skipped`. `--keep` leaves lifecycle environments in place.

## Tests in a module

A [module](modules.md) carries tests:

```
acme-nginx/
  meridian.yaml
  documents/proxy.yaml
  data/defaults.yaml
  tests/proxy.test.yaml
```

Paths are relative to the test. A project can test an installed module too:

```yaml
spec:
  documents: ["@acme/nginx:documents/proxy.yaml"]
  data: ["@acme/nginx:data/defaults.yaml"]
```

`module install` never runs tests.

## In a pipeline

Run before the plan ([pipelines](../operations/running-in-a-pipeline.md)):

```yaml
- run: meridian test --junit meridian-tests.xml
- uses: dorny/test-reporter@v1
  if: always()
  with:
    name: meridian tests
    path: meridian-tests.xml
    reporter: java-junit
```

## What is not here yet

- **`break` and `diagnose` phases** ([issue 237](https://github.com/ZanattaMichael/meridian-core/issues/237)).
- **A run id** (`${{ meridian.run.id }}`) for names and tags, so lifecycle cases can run side
  by side ([issue 235](https://github.com/ZanattaMichael/meridian-core/issues/235)).
- **Disposable machines** ([issue 123](https://github.com/ZanattaMichael/meridian-core/issues/123)).
- **Expectations on events.**
- **Line numbers** for test-file mistakes (paths only, such as `spec.cases[2]`).

See [decision 0037](../decisions/0037-a-test-is-a-document-of-cases-over-compile-and-plan.md)
and [decision 0050](../decisions/0050-a-lifecycle-case-provisions-verifies-and-always-destroys.md).
