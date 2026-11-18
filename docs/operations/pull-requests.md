# A saved plan, and applying it from a pull request

<p align="center"><img src="../graphics/meridian-saved-plan.svg" alt="A saved plan: meridian plan --out writes a plan file holding each stage's artifact hash, its changes and the tool's own saved plan; the reviewer reads it on the pull request; meridian apply --plan compiles again, refuses when a hash or the state has moved, and hands each tool the plan it saved" width="100%"></p>

`meridian plan` says what an apply would change, and `meridian apply` plans
again before it applies. Nothing ties the two together: the documents, the
data or the world can change in between, and what a reviewer approved is not
necessarily what is applied. A **saved plan** ties them. `plan --out` writes
one file holding what was planned, and `apply --plan` carries out exactly
that, or refuses.

The pull request automation is built on it: a plan on every push, posted as
one comment that is updated in place, saved as an artifact, and applied after
the merge or the approval by a job that can apply nothing else. The GitHub
Actions half is two [actions](#github-actions) run for real on every change;
the [GitLab recipe](#gitlab-ci) does the same with jobs.

## Contents

- [The plan file](#the-plan-file)
- [What `apply --plan` checks](#what-apply---plan-checks)
- [Which tools save a plan](#which-tools-save-a-plan)
- [GitHub Actions](#github-actions) (executed in CI)
- [GitLab CI](#gitlab-ci)
- [What a plan file holds, and what it never holds](#what-a-plan-file-holds-and-what-it-never-holds)

## The plan file

```sh
meridian plan site.yaml -l env=prod --out plan.mplan
meridian apply --plan plan.mplan
```

`plan --out FILE` runs the plan as always and then writes the file, one JSON
object, with:

- what the plan was asked: the documents, every `--data` file and the hash
  of its bytes, every `--set`, the selector, and the command line;
- the id of the run that took it, which the apply's record then names;
- one entry per stage: its name and kind, the target, the **hash of the
  artifact** the document compiled to, the changes the tool reported, the
  policy's decision when one was asked, the hash of what was last applied to
  the stage at the time, and the **tool's own saved plan** where its plugin
  saves one (base64, as the tool wrote it).

The file is written only when every stage planned. A stage that failed, or
that the policy denied, leaves nothing to apply, so the command says so and
exits `1` with no file.

`apply --plan FILE` takes what was asked from the file: the documents, the
data, the values and the selector may all be left off the command line, and
any that is given must be what the plan was taken with, so the apply is of
the plan and not of something like it. `--resume` cannot be combined with it:
a saved plan is applied whole.

With `--output json` both commands carry a `plan` object: the file, the
plan's run (on an apply) and whether it has changes
([the run object](../reference/cli-json.md#compile-plan-apply-drift-rollback)).
A plan file is for one apply: once applied, the state has moved on, and the
same file is refused.

## What `apply --plan` checks

Before anything runs, the whole plan is held against the run:

- **the same stages**: every stage the run would apply is in the plan, and
  every stage in the plan is in the run; a plan taken for other documents or
  another selection is refused;
- **nothing applied since**: for every stage, what the run-state says was
  last applied is what it said when the plan was taken. A stage applied by
  any run in between, succeeded or failed, makes the plan stale and it is
  refused.

A refusal here stops the run before it holds anything but the lock:
`refused` in the output, `kind: plan`, exit `1`.

Then, stage by stage, as each is about to be applied:

- **the same artifact**: the document is compiled again, with the apply's
  data and secrets, and must compile to the very artifact hash the plan
  recorded. If it does not, the stage fails with the reason `plan-differs`,
  the stages after it are not run, and the stages before it stay applied,
  as after any failed stage. A stage the plan previewed and whose own
  `spec.when` now leaves out fails the same way. When a `--data` file's
  bytes are not what the plan recorded, the failure names it: `... and the
  data file prod.yaml has changed since the plan`. A changed data file is
  never a refusal on its own: a key no stage reads can change, and the
  artifacts, and so the plan, still hold.
- **the tool's own plan**: where the plugin saved one, the tool is handed it
  back and applies that and nothing else. Terraform and OpenTofu apply the
  plan file and refuse a stale one themselves; Pulumi constrains its update
  to the saved plan; CloudFormation executes the change set the plan kept.
  Where there is none, the apply plans again, with the artifact hash as the
  check.

A stage the plan could not preview, because it waited on an output that did
not exist yet, is recorded as `deferred` in the file. The apply compiles and
applies it as any apply would, and warns that it was not checked against the
plan. The report and the file say which stages that is: in a workflow whose
second stage waits on the first, reviewing the plan means reviewing the
first stage and the second's documents.

## Which tools save a plan

| Target | Saved plan | Not saved when |
|---|---|---|
| `terraform`, `opentofu` | the `-out` plan file, applied with `terraform apply <file>` | the run handed over a secret: a Terraform plan file holds every variable's value, so it is scrubbed instead, and the apply plans again |
| `pulumi` | the update plan of `preview --save-plan`, applied with `up --plan` | the run handed over a secret, for the same reason |
| `cloudformation` | the change set, which is not discarded; the file holds its name | never: the change set's values stay with CloudFormation |
| every other target | none | the apply checks the artifact hash and plans again |

The [capability matrix](../reference/capability-matrix.md) has the column.
A plugin says so with `Runner::saves_plans`
([writing a plugin](../contributing/writing-a-plugin.md#saved-plans)).

## GitHub Actions

Executed on every change by
[`saved-plan.yml`](https://github.com/ZanattaMichael/meridian-core/blob/main/.github/workflows/saved-plan.yml),
against the fake target: the plan action plans and comments on the pull
request, the apply action applies the plan in a second job, and a third job
proves a plan whose document changed since is refused.

Two composite actions live in this repository:

- [`.github/actions/plan`](https://github.com/ZanattaMichael/meridian-core/tree/main/.github/actions/plan)
  runs `meridian plan --out`, writes the Markdown report, posts it as one
  comment on the pull request that is found again by its marker and edited
  in place, and keeps the plan file, the report and the state directory as
  one artifact. Its outputs: `has-changes`, `plan-file` and `run`.
- [`.github/actions/apply`](https://github.com/ZanattaMichael/meridian-core/tree/main/.github/actions/apply)
  takes that artifact and runs `meridian apply --plan` on it. The documents,
  data and selector come from the plan, so the job cannot be pointed at
  anything else.

Both expect `meridian` and its plugins on the `PATH`, as the install step of
[Running Meridian in a pipeline](running-in-a-pipeline.md#the-shape-of-every-job)
puts them, or take a release version in `install`.

<!-- github-actions-snippet -->
```yaml
name: infrastructure

on:
  pull_request:
  push:
    branches: [main]

permissions:
  contents: read
  pull-requests: write     # the plan comment
  id-token: write          # the job's OIDC identity, for credentials declared from: ambient

# One run at a time: the plan's record is carried to the apply.
concurrency:
  group: meridian-${{ github.ref }}

env:
  MERIDIAN_PLUGIN_SIGNATURES: require
  SITE_DB_PASSWORD: ${{ secrets.SITE_DB_PASSWORD }}

jobs:
  plan:
    runs-on: ubuntu-latest
    outputs:
      has-changes: ${{ steps.plan.outputs.has-changes }}
    steps:
      - uses: actions/checkout@v4
      - id: plan
        uses: ZanattaMichael/meridian-core/.github/actions/plan@v0.1.0
        with:
          install: '0.1.0'
          files: site.yaml
          args: -l env=prod --policy policies/

  apply:
    needs: plan
    if: github.ref == 'refs/heads/main' && needs.plan.outputs.has-changes == 'true'
    runs-on: ubuntu-latest
    environment: production     # reviewers and the pause before production live here
    steps:
      - uses: actions/checkout@v4
      - uses: ZanattaMichael/meridian-core/.github/actions/apply@v0.1.0
        with:
          install: '0.1.0'
```

On a pull request the plan is the comment; on `main` the same plan is
taken again from the merged tree and applied behind the environment's
approval. The plan the reviewers read and the plan that is applied are two
runs: a pull request's plan is taken against its own tree and state, and a
merge can change both, so the apply is of the plan `main` took. With a
[shared backend](running-in-a-pipeline.md#sharing-the-record-between-jobs)
the `.meridian/` directory in the artifact is not needed and two pipelines
cannot apply the same documents at once.

The comment holds the report `--report markdown` writes: the stages, their
changes and the policy's decision. No value of the run is in it, as none is
in any report ([what a plan file holds](#what-a-plan-file-holds-and-what-it-never-holds)).

## GitLab CI

Reviewed, not executed. The plan job keeps the file as an artifact; a merge
request shows the Markdown report among the job's artifacts, and the apply
job, manual and behind a protected environment, takes the file from the job
that made it.

```yaml
stages: [plan, apply]

plan:
  stage: plan
  script:
    - meridian plan site.yaml -l env=prod --out meridian.mplan --report markdown=meridian-plan.md
  artifacts:
    when: always
    paths: [meridian.mplan, meridian-plan.md, .meridian/]
    expire_in: 1 week

apply:
  stage: apply
  needs: [plan]
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH
      when: manual               # the approval
  environment:
    name: production
  script:
    - meridian apply --plan meridian.mplan
  artifacts:
    when: always
    paths: [.meridian/]
```

The install and identity steps are those of the
[GitLab snippet](running-in-a-pipeline.md#gitlab-ci) and are left out here.

## What a plan file holds, and what it never holds

The file holds names, hashes and one-line summaries: a stage's name, the
tool's one-line account of each change (`create web_vm: aws_instance`), the
hash of an artifact and never the artifact. The one part that is the tool's
own, the saved plan, is kept out of the file when it would carry a value:
Terraform's and OpenTofu's plan files hold every variable, so a run that
handed over a secret saves none; Pulumi's update plan holds every input, so
the same; CloudFormation's change set stays with CloudFormation and only its
name is kept.

Commit the file nowhere. It is for one apply, it names a run, and it is
stale the moment anything is applied. Keep it as the pipeline's artifact,
which is what the actions do.
