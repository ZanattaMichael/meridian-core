# An apply from a saved plan applies that plan or nothing, and the file never holds a value

| | |
|---|---|
| Area | Orchestrator |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | issue 115, a saved plan and pull request automation built on it |
| Where it shows | [A saved plan, and applying it from a pull request](../operations/pull-requests.md), [`plan --out`](../reference/cli.md#plan), [`apply --plan`](../reference/cli.md#apply), `RunContext.save_plan` and `saved_plan` in the plugin protocol |

## Context

`meridian plan` and `meridian apply` were two runs with nothing between
them: the apply planned again, against whatever the documents, the data and
the world had become. A reviewer who approved a plan on a pull request had
approved a run that would never happen as such. Terraform, OpenTofu, Pulumi
and CloudFormation each have a saved plan of their own; the other targets
have none, and Meridian's job is to make one plan of all of them.

## The choice

`plan --out` writes one file for the whole workflow, and `apply --plan`
either carries it out or refuses. Four things were decided:

1. **The artifact hash is the check, and the tool's plan is the delivery.**
   Every stage is compiled again at apply and must compile to the hash the
   plan recorded; that holds for every target, whether or not its tool
   saves a plan. Where the tool does, the plugin is handed its plan back
   and applies that and nothing else. The check is Meridian's, the exact
   delivery is the tool's, and a target without a saved plan loses only
   the second. The comparison already existed: it is what `--resume` uses
   to leave a stage alone.
2. **A stage applied since the plan makes the whole plan stale.** The file
   records what the run-state said was last applied to each stage, by any
   run, succeeded or failed, and the apply refuses before it runs anything
   when that has moved. The alternative, applying the stages that still
   match, would apply half a reviewed plan.
3. **The file never holds a value.** It holds names, hashes and the tool's
   one-line change summaries, which no report holds a value in either. The
   one tool-owned part, the saved plan, is left out whenever it would carry
   one: Terraform's and OpenTofu's plan files hold every variable, Pulumi's
   update plan every input, so a run that handed over a secret saves none
   and the apply plans again for that stage, which the file says. The
   CloudFormation change set stays with CloudFormation and only its name is
   kept. The file is therefore safe to keep as a pipeline artifact and to
   hand between jobs, which is where it lives.
4. **What was asked comes from the file.** `apply --plan` takes the
   documents, data, values and selector from the plan, and refuses any that
   is typed differently. An apply job in a pipeline can then be given the
   plan and nothing else, and cannot be pointed at other documents.
5. **A data file's hash explains, and never refuses.** The file records
   the hash of each `--data` file's bytes. A data file that changed is
   already caught by the artifact of every stage that reads it; the hash
   only lets the `plan-differs` failure name the file. Refusing on the hash
   alone would throw out a plan that still holds whenever a key no stage
   reads was edited, such as another environment's value.

A stage the plan could not preview, because it waited on an output that
did not exist yet, is applied unchecked, with a warning: refusing it would
make a saved plan useless for exactly the two-stage workflows Meridian is
for, and the file and the report both say which stages were not checked.

## Alternatives

- **Handing the tool its saved plan and skipping Meridian's own
  compile.** Rejected: only four targets have one, and the document is the
  source; a plan that was taken from documents that have since changed is
  not the reviewer's plan, whatever the tool's file says.
- **Applying only the stages that still match, and refusing the rest.**
  Rejected: a reviewed plan is one thing. A stage that differs stops the
  run as any failed stage does, with the reason `plan-differs`, and the
  stages before it stay applied, as after any failure.
- **A plan file that is also the signed bundle of compiled files
  (issue 121).** Not done here; the file records hashes, not artifacts, so
  a bundle can carry the artifacts beside it later without changing the
  file's meaning.
- **Positional `meridian apply plan.mplan`.** Rejected: a file after
  `apply` is a document, and an option says what the file is.

## Status

Proposed. The owner agreed on 2026-11-18 that a plan with a `deferred`
stage is applied with a warning rather than refused, and that the file
carries each data file's hash to name it as the cause, never to refuse on
it.
