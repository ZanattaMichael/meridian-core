# A test is a document of cases over compile and plan, and never a script

| | |
|---|---|
| Area | Testing |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | issue 114, `meridian test` |
| Where it shows | [Testing documents and modules](../reference/testing.md), [`meridian test`](../reference/cli.md#test), [Machine-readable output](../reference/cli-json.md#test), [Modules](../reference/modules.md#testing-a-module) |

## Context

Meridian's own test suite is thorough, but a person who writes documents or
publishes a module had no way to test their work beyond `validate`, `lint`
and `plan`. Terraform's `terraform test` and OpenTofu's `tofu test` put `run`
and `assert` blocks in a file of their own language; Pulumi tests in the
host language; configuration management tests with Molecule, Test Kitchen
and Goss, which converge a machine and assert on it. Issue 114 asked for
three levels, compile, plan and apply, and warned against inventing a test
language.

## The choice

A test is a document, `kind: Test`, with the same `apiVersion` and
`metadata` as every other document. It names the documents under test and a
list of cases; each case gives data and a level, and checks what came out
through structured expectations (`outcome`, `rule`, `files`, `changes`) and
assertions written in the one expression language, over a value `stages`
that holds what each document came to. There is no `run` block that applies,
no hook that runs a command, and no way to change a document from a test.

Two levels are built. A **compile** case is `meridian compile` with the
case's data; a **plan** case is `meridian plan` in a scratch state directory
of the case's own, emptied first, so a plan never sees what another case or
the project itself applied. An **apply** case is not built: it needs a
`destroy` operation and somewhere disposable to apply to, and both are
tracked elsewhere.

The command reads every test file before running any case, and refuses a
case that checks nothing, an assertion that reads a name it cannot read, and
an outcome word that is not one of the six, so a mistake in a test is found
before the tests take their time.

Test files are `*.test.yaml` under `tests/` when no file is named, and
`lint`, `fmt` and the language server leave a file of kind `Test` alone, as
they do a module's manifest. A module carries its tests under `tests/` and
names its own files relative to the test; `module install` neither fetches
nor runs them.

The results are text for a person, one JSON object for a machine, and JUnit
XML for a pipeline engine, written by the command itself rather than through
the run reports, because a test is not a run: it keeps no record and holds no
lock.

## Alternatives

- **A test language of its own**, as Terraform has. It would be a second
  grammar to document and to teach an editor, for conditions the expression
  language already writes.
- **Tests in Rust**, through the plugin test kit. They test a plugin, not a
  document, and need a toolchain a module author does not have.
- **A `Test` kind inside `meridian-ir`**, parsed and validated with the other
  two kinds. It would make every command that loads a bundle refuse or skip
  it, and the language server describe a kind it does not compile. A test is
  read by one command, so it is parsed by that command's crate, from the same
  YAML reader.
- **Recording each case as a run**, with the lock and the record. A test
  changes nothing, and a hundred test runs in the record would bury the
  applies that matter.
- **The refusal's rule from the stage report.** The orchestrator reports a
  refusal as text; the test reads the rule and the resource back out of that
  text, as `meridian compile` prints it. Carrying them as fields of the
  report would be cleaner and is a small follow-up.

## What the owner may change

The names of the fields (`expect`, `assert`, `changeCount`), the six outcome
words, and whether a case with no checks should be allowed as a smoke test.
