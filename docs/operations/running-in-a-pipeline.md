# Running Meridian in a pipeline

<p align="center"><img src="../graphics/meridian-in-a-pipeline.svg" alt="Meridian inside a pipeline engine: the engine's band (trigger, runner and identity, secret release, approval, logs and artifacts) above Meridian's band (workflow, orchestrator, target plugins, secret delivery); the engine's job runs meridian apply, and Meridian's events come back as annotations and a job summary" width="100%"></p>

Every pipeline engine can run Meridian as one job, today, with no plugin and
no new language: the job runs `meridian apply`, and Meridian does the rest.
This page is the one to copy from. It says what the engine owns and what
Meridian owns, gives a snippet for each engine, and describes what `meridian`
does differently when it finds itself inside one.

The GitHub Actions snippet is run for real on every change, against the
fake target, by [`.github/workflows/pipeline-shell.yml`](https://github.com/ZanattaMichael/meridian-core/blob/main/.github/workflows/pipeline-shell.yml):
two jobs sharing one record, a plan, an apply, a failure, and
`apply --resume`. A test holds the page to that workflow, command by command.
**The other snippets were reviewed by hand and have not been executed**; each
says so where it starts. If one of them is wrong for your engine, an issue
with the fix is welcome.

## Contents

- [Who owns what](#who-owns-what)
- [The shape of every job](#the-shape-of-every-job)
- [GitHub Actions](#github-actions) (executed in CI)
- [Azure Pipelines](#azure-pipelines)
- [GitLab CI](#gitlab-ci)
- [Buildkite](#buildkite)
- [Jenkins](#jenkins)
- [Argo Workflows](#argo-workflows)
- [Tekton](#tekton)
- [Octopus Deploy](#octopus-deploy)
- [What `meridian` does inside an engine](#what-meridian-does-inside-an-engine)
- [Checking the job's identity first](#checking-the-jobs-identity-first)
- [Sharing the record between jobs](#sharing-the-record-between-jobs)
- [Approvals](#approvals)

[A saved plan, and applying it from a pull request](pull-requests.md) builds
on these jobs: a plan on every push, posted as one comment, and applied
after the approval by a job that can apply nothing else.

## Who owns what

An engine and Meridian divide the work along one line: the engine decides
*when and as whom* a run happens and keeps what the run produced; Meridian
decides *what* happens and in what order, and keeps the record of it.

| The engine owns | Meridian owns |
|---|---|
| **Triggers**: a push, a pull request, a schedule, a button | **Order**: Infrastructure before ResourceSets, imports before the documents that read them, one run at a time on the [lock](../concepts/run-safety.md#one-run-at-a-time) |
| **Runners and their identity**: where the job runs, and the OIDC token or service connection it runs as | **Secrets delivery**: a secret reaches the tool by a [private channel](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) and is never on a command line, in a file the engine keeps, or in a log |
| **Secret release**: which secrets a job may read, and when | **The record**: every run's [events](../reference/state.md#events) and run-state, in a backend the next job can reach |
| **Environments, reviewers and approvals**: who may deploy to production, and the pause before it | **Resume and rollback**: `apply --resume` continues a failed apply from the step that failed; `--on-failure rollback` and `meridian rollback` put one back |
| **Logs and artifacts**: the job's log, its summary page, and the files it keeps | **Drift and policy**: `meridian drift` says whether what was applied is still there; `--policy` asks a policy before anything is applied |

The engine never sees a value Meridian holds, so there is nothing for it to
mask: no secret is ever printed, by construction
([how a secret is held](../concepts/secrets-in-memory.md)). Rely on that,
not on the engine's masking, which only hides what it already knows.

## The shape of every job

The snippets below all do the same six things. Read them once here, and the
snippets become easy to follow.

1. **Install a pinned `meridian` and its plugins** from the release's files,
   and check what was downloaded. The [install script](../quickstart/install.md#linux-and-macos)
   takes `MERIDIAN_VERSION`, and with `MERIDIAN_VERIFY=cosign` refuses to
   install unless the release's signature verifies. A released `meridian`
   already starts only [signed plugins](../reference/plugin-signatures.md);
   `MERIDIAN_PLUGIN_SIGNATURES=require` in the job's environment says so
   out loud, and makes a build from source behave the same way.
2. **Pin the modules**: `meridian module install --locked` fails if
   `meridian.lock` would have to change, so a job never fetches a module
   the repository did not pin.
3. **Check before running**: `meridian validate` reads the documents and
   loads no plugin; `meridian secret check` says whether every declared
   secret, and every credential declared `from: ambient`, would be found
   on this runner, without reading a value.
4. **`plan` on a pull request, `apply` on the protected branch.** With
   `--detailed-exitcode`, a plan exits `2` when there is something to apply,
   so a job can tell "nothing to do" from "changes waiting".
5. **Share the record**: the lock, the run-state and the runs must be where
   the next job, on another runner, can reach them, so a failed apply can be
   resumed and a plan can see the last apply. See
   [sharing the record between jobs](#sharing-the-record-between-jobs).
6. **Keep the events**: `--events json` writes every event of the run to
   standard output, one per line, for the engine to keep as an artifact.
   `meridian report --from FILE` makes a report from that file anywhere,
   later. The report for people moves to standard error, where the job's log
   shows it.

Secrets reach `spec.secrets` through the [`env` provider](../reference/dsl/secrets.md#the-providers):
the engine puts the secret in the job's environment, the document names the
variable, and Meridian removes the variable from its own environment as soon
as it has read it, so no plugin or tool started afterwards inherits it. The
engine's identity reaches `spec.credentials` as
[`from: ambient`](../reference/dsl/credentials.md#from-ambient): the engine's
own action or service connection signs the job in, and the tool finds that
sign-in as it would on a laptop. No value is in any file the engine keeps.

```yaml
spec:
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD        # set by the engine from its secret store
  credentials:
    - name: deployer
      type: aws
      from: ambient                # the job's OIDC identity, exchanged by the engine
```

## GitHub Actions

Executed on every change by `pipeline-shell.yml`, against the fake target.
`id-token: write` is what lets the job exchange its OIDC token for a cloud
identity; `aws-actions/configure-aws-credentials`, `azure/login` and
`google-github-actions/auth` each leave a sign-in that `from: ambient` finds.
Two jobs share one record: `plan` keeps the state directory as an artifact,
`apply` takes it back. The `concurrency` group keeps two runs of the
workflow from racing for it.

<!-- github-actions-snippet -->
```yaml
name: infrastructure

on:
  pull_request:
  push:
    branches: [main]
  workflow_dispatch:
    inputs:
      resume:
        description: continue the apply that failed, from the step that failed
        type: boolean
        default: false

permissions:
  contents: read
  id-token: write          # the job's OIDC identity, for credentials declared from: ambient

# One run of this workflow at a time: the record is carried between its jobs.
concurrency:
  group: meridian-${{ github.ref }}

env:
  MERIDIAN_VERSION: '0.1.0'
  MERIDIAN_VERIFY: cosign
  MERIDIAN_PLUGIN_SIGNATURES: require
  SITE_DB_PASSWORD: ${{ secrets.SITE_DB_PASSWORD }}

jobs:
  plan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install a pinned Meridian, checking the release's signature
        run: |
          curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh
          echo "$HOME/.meridian/bin" >> "$GITHUB_PATH"
      - name: Pin the modules
        run: meridian module install --locked
      - name: Check the documents, the secrets and the job's identity
        run: |
          meridian validate site.yaml
          meridian secret check site.yaml
      - name: Plan
        run: |
          mkdir -p events
          meridian plan site.yaml --events json > events/plan.jsonl
      - name: Keep the events and the record
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: meridian-plan
          path: |
            events/
            .meridian/
          include-hidden-files: true

  apply:
    needs: plan
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-latest
    environment: production     # reviewers and the pause before production live here
    steps:
      - uses: actions/checkout@v4
      - name: Install a pinned Meridian, checking the release's signature
        run: |
          curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh
          echo "$HOME/.meridian/bin" >> "$GITHUB_PATH"
      - name: Take the record from the plan job
        uses: actions/download-artifact@v4
        with:
          name: meridian-plan
      - name: Apply
        if: ${{ !inputs.resume }}
        run: meridian apply site.yaml --events json > events/apply.jsonl
      - name: Continue the apply that failed
        if: ${{ inputs.resume }}
        run: meridian apply site.yaml --resume --events json > events/resume.jsonl
      - name: Keep the events and the record
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: meridian-apply
          path: |
            events/
            .meridian/
          include-hidden-files: true
```

What the job page shows, with nothing more in the workflow: a refused
document as an annotation at its line, a failed step as an annotation at its
document, and the run's report as the step summary. When the apply fails, fix
the cause, then run the workflow by hand with `resume` ticked: the job takes
the record of the failed run from the artifact and continues from the step
that failed.

## Azure Pipelines

Reviewed, not executed. A [workload identity federation service
connection](https://learn.microsoft.com/azure/devops/pipelines/library/connect-to-azure)
signs the job in without a secret; `AzureCLI@2` leaves the Azure CLI logged
in, which `from: ambient` finds. The record is carried between stages as a
pipeline artifact.

```yaml
trigger:
  branches:
    include: [main]
pr:
  branches:
    include: ['*']

variables:
  MERIDIAN_VERSION: '0.1.0'
  MERIDIAN_VERIFY: cosign
  MERIDIAN_PLUGIN_SIGNATURES: require

stages:
  - stage: plan
    jobs:
      - job: plan
        pool: { vmImage: ubuntu-latest }
        steps:
          - checkout: self
          - bash: |
              curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh
              echo "##vso[task.prependpath]$HOME/.meridian/bin"
            displayName: Install a pinned Meridian
          - bash: meridian module install --locked
            displayName: Pin the modules
          - task: AzureCLI@2
            displayName: Check, then plan, as the service connection's identity
            inputs:
              azureSubscription: production          # a workload identity federation connection
              scriptType: bash
              scriptLocation: inlineScript
              inlineScript: |
                meridian validate site.yaml
                meridian secret check site.yaml
                mkdir -p events
                meridian plan site.yaml --events json > events/plan.jsonl
            env:
              SITE_DB_PASSWORD: $(SITE_DB_PASSWORD)   # a secret variable, from a variable group or Key Vault
          - publish: .meridian
            artifact: meridian-record
            condition: always()
          - publish: events
            artifact: meridian-events
            condition: always()

  - stage: apply
    dependsOn: plan
    condition: and(succeeded(), eq(variables['Build.SourceBranch'], 'refs/heads/main'))
    jobs:
      - deployment: apply
        environment: production                     # approvals and checks live on the environment
        pool: { vmImage: ubuntu-latest }
        strategy:
          runOnce:
            deploy:
              steps:
                - checkout: self
                - bash: |
                    curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh
                    echo "##vso[task.prependpath]$HOME/.meridian/bin"
                  displayName: Install a pinned Meridian
                - download: current
                  artifact: meridian-record
                - bash: mv "$(Pipeline.Workspace)/meridian-record" .meridian
                  displayName: Take the record from the plan stage
                - task: AzureCLI@2
                  displayName: Apply
                  inputs:
                    azureSubscription: production
                    scriptType: bash
                    scriptLocation: inlineScript
                    inlineScript: |
                      mkdir -p events
                      meridian apply site.yaml --events json > events/apply.jsonl
                  env:
                    SITE_DB_PASSWORD: $(SITE_DB_PASSWORD)
                - publish: events
                  artifact: meridian-apply-events
                  condition: always()
```

Inside Azure Pipelines, `meridian` writes each problem as a
`##vso[task.logissue]` line, which the job shows as an issue, and the run's
report as a summary tab through `task.uploadsummary`. A re-run after a
failure is `meridian apply site.yaml --resume`, with the record from the
failed run downloaded first; a pipeline that takes a `resume` parameter can
choose between the two commands as the GitHub snippet does.

## GitLab CI

Reviewed, not executed. `id_tokens` gives the job an OIDC token that the
cloud's CLI exchanges for an identity. GitLab has no annotation syntax and
no job summary; the JUnit report shows the steps on the merge request as
tests, and the `markdown` report is a file to read from the artifacts.

```yaml
variables:
  MERIDIAN_VERSION: '0.1.0'
  MERIDIAN_VERIFY: cosign
  MERIDIAN_PLUGIN_SIGNATURES: require

default:
  image: ubuntu:24.04
  before_script:
    - apt-get update -qq && apt-get install -y -qq curl ca-certificates
    - curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh
    - export PATH="$HOME/.meridian/bin:$PATH"
    - meridian module install --locked

stages: [plan, apply]

plan:
  stage: plan
  id_tokens:
    AWS_WEB_IDENTITY_TOKEN: { aud: sts.amazonaws.com }      # for an aws credential declared from: ambient
  variables:
    AWS_ROLE_ARN: arn:aws:iam::123456789012:role/deploy
  script:
    - printf '%s' "$AWS_WEB_IDENTITY_TOKEN" > "$CI_PROJECT_DIR/.oidc-token"
    - export AWS_WEB_IDENTITY_TOKEN_FILE="$CI_PROJECT_DIR/.oidc-token"
    - meridian validate site.yaml
    - meridian secret check site.yaml
    - mkdir -p events
    - meridian plan site.yaml --events json --report junit=meridian-plan.xml --report markdown=meridian-plan.md > events/plan.jsonl
  artifacts:
    when: always
    paths: [events/, .meridian/, meridian-plan.md]
    reports:
      junit: meridian-plan.xml

apply:
  stage: apply
  needs: [plan]
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH
  environment:
    name: production            # protected environments hold the approval
  id_tokens:
    AWS_WEB_IDENTITY_TOKEN: { aud: sts.amazonaws.com }
  variables:
    AWS_ROLE_ARN: arn:aws:iam::123456789012:role/deploy
  script:
    - printf '%s' "$AWS_WEB_IDENTITY_TOKEN" > "$CI_PROJECT_DIR/.oidc-token"
    - export AWS_WEB_IDENTITY_TOKEN_FILE="$CI_PROJECT_DIR/.oidc-token"
    - mkdir -p events
    - meridian apply site.yaml --events json --report junit=meridian-apply.xml > events/apply.jsonl
  artifacts:
    when: always
    paths: [events/, .meridian/]
    reports:
      junit: meridian-apply.xml
```

The `.oidc-token` file holds the job's OIDC token, which is the engine's
own and short-lived; it is not a Meridian secret, and the job's
`SITE_DB_PASSWORD` comes from a masked CI variable as on every engine.
`.meridian/` is passed from `plan` to `apply` as an artifact; a
[shared backend](#sharing-the-record-between-jobs) is the better answer once
more than one pipeline touches the same documents.

## Buildkite

Reviewed, not executed. Buildkite runs each job's command in a pseudo-terminal
by default, which makes `meridian` think a person is watching; set
`MERIDIAN_ANNOTATIONS=on` so it writes for the engine anyway. The report
reaches the build page through `buildkite-agent annotate`, which `meridian`
runs itself when the agent is on the `PATH`. The OIDC token comes from
`buildkite-agent oidc request-token`.

```yaml
env:
  MERIDIAN_VERSION: '0.1.0'
  MERIDIAN_VERIFY: cosign
  MERIDIAN_PLUGIN_SIGNATURES: require
  MERIDIAN_ANNOTATIONS: 'on'

steps:
  - label: ':mag: plan'
    key: plan
    commands:
      - curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh
      - export PATH="$HOME/.meridian/bin:$PATH"
      - meridian module install --locked
      - buildkite-agent oidc request-token --audience sts.amazonaws.com > .oidc-token
      - export AWS_WEB_IDENTITY_TOKEN_FILE="$PWD/.oidc-token" AWS_ROLE_ARN=arn:aws:iam::123456789012:role/deploy
      - meridian validate site.yaml
      - meridian secret check site.yaml
      - mkdir -p events
      - meridian plan site.yaml --events json > events/plan.jsonl
    artifact_paths:
      - events/**
      - .meridian/**
    plugins:
      - secrets#v1.0.0:                      # or the agent's secrets hook: a value into SITE_DB_PASSWORD
          env:
            SITE_DB_PASSWORD: site-db-password

  - block: ':rocket: apply to production'   # the approval is the engine's
    if: build.branch == 'main'

  - label: ':rocket: apply'
    depends_on: plan
    if: build.branch == 'main'
    commands:
      - curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh
      - export PATH="$HOME/.meridian/bin:$PATH"
      - buildkite-agent artifact download '.meridian/**' . --step plan
      - buildkite-agent oidc request-token --audience sts.amazonaws.com > .oidc-token
      - export AWS_WEB_IDENTITY_TOKEN_FILE="$PWD/.oidc-token" AWS_ROLE_ARN=arn:aws:iam::123456789012:role/deploy
      - mkdir -p events
      - meridian apply site.yaml --events json > events/apply.jsonl
    artifact_paths:
      - events/**
      - .meridian/**
    plugins:
      - secrets#v1.0.0:
          env:
            SITE_DB_PASSWORD: site-db-password
```

## Jenkins

Reviewed, not executed. A declarative pipeline, with the secret from the
credentials store and the record stashed between stages; on one agent, the
workspace itself carries it. `JENKINS_URL` is how `meridian` knows where it
is. Jenkins has no annotation syntax of its own; the JUnit report puts the
steps on the build page, and the `html` report is a page to publish.

```groovy
pipeline {
  agent any
  environment {
    MERIDIAN_VERSION = '0.1.0'
    MERIDIAN_VERIFY = 'cosign'
    MERIDIAN_PLUGIN_SIGNATURES = 'require'
    PATH = "${env.HOME}/.meridian/bin:${env.PATH}"
    SITE_DB_PASSWORD = credentials('site-db-password')
  }
  stages {
    stage('install') {
      steps {
        sh 'curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh'
        sh 'meridian module install --locked'
      }
    }
    stage('plan') {
      steps {
        sh 'meridian validate site.yaml'
        sh 'meridian secret check site.yaml'
        sh 'mkdir -p events && meridian plan site.yaml --events json --report junit=meridian-plan.xml > events/plan.jsonl'
      }
      post {
        always {
          junit 'meridian-plan.xml'
          archiveArtifacts artifacts: 'events/**', allowEmptyArchive: true
          stash name: 'record', includes: '.meridian/**', allowEmpty: true
        }
      }
    }
    stage('apply') {
      when { branch 'main' }
      input { message 'Apply to production?' }     // the approval is the engine's
      steps {
        unstash 'record'
        sh 'mkdir -p events && meridian apply site.yaml --events json --report junit=meridian-apply.xml --report html=meridian-apply.html > events/apply.jsonl'
      }
      post {
        always {
          junit 'meridian-apply.xml'
          archiveArtifacts artifacts: 'events/**, meridian-apply.html', allowEmptyArchive: true
        }
      }
    }
  }
}
```

The cloud identity on Jenkins is whatever the agent has: an instance role, a
credentials-binding step that exports the cloud's variables, or an OIDC
plugin. `from: ambient` finds any of them, and `meridian secret check` says
which.

## Argo Workflows

Reviewed, not executed. A workflow template whose steps run the released
container image; the record lives on a volume the steps share, and the
events are an output artifact. Argo sets `ARGO_*` variables in the
container, which is how `meridian` knows where it is. The cloud identity is
the pod's: a service account with workload identity (IRSA, Azure Workload
Identity, GKE Workload Identity) leaves the token file `from: ambient`
finds.

```yaml
apiVersion: argoproj.io/v1alpha1
kind: WorkflowTemplate
metadata:
  name: meridian-apply
spec:
  entrypoint: run
  serviceAccountName: meridian-deployer        # bound to a cloud identity through workload identity
  volumeClaimTemplates:
    - metadata: { name: record }
      spec:
        accessModes: [ReadWriteOnce]
        resources: { requests: { storage: 1Gi } }
  arguments:
    parameters:
      - name: resume
        value: 'false'
  templates:
    - name: run
      steps:
        - - name: plan
            template: meridian
            arguments:
              parameters: [{ name: command, value: 'plan' }]
        - - name: approve
            template: approve                  # a suspend step: the approval is the engine's
        - - name: apply
            template: meridian
            arguments:
              parameters: [{ name: command, value: 'apply' }]
            when: '{{workflow.parameters.resume}} == false'
          - name: resume
            template: meridian
            arguments:
              parameters: [{ name: command, value: 'apply --resume' }]
            when: '{{workflow.parameters.resume}} == true'
    - name: approve
      suspend: {}
    - name: meridian
      inputs:
        parameters: [{ name: command }]
        artifacts:
          - name: documents
            path: /work
            git: { repo: https://github.com/example/infrastructure.git, revision: main }
      container:
        image: ghcr.io/zanattamichael/meridian:0.1.0
        workingDir: /work
        command: [sh, -ec]
        args:
          - |
            mkdir -p events
            meridian module install --locked
            meridian validate site.yaml
            meridian secret check site.yaml
            meridian {{inputs.parameters.command}} site.yaml --events json > events/run.jsonl
        env:
          - name: MERIDIAN_PLUGIN_SIGNATURES
            value: require
          - name: MERIDIAN_STATE_DIR
            value: /record
          - name: SITE_DB_PASSWORD
            valueFrom:
              secretKeyRef: { name: site, key: db-password }
        volumeMounts:
          - { name: record, mountPath: /record }
      outputs:
        artifacts:
          - name: events
            path: /work/events
```

## Tekton

Reviewed, not executed. One task with a step per command, a workspace for
the record, and a result for the run. Tekton sets `TEKTON_*` variables in
each step's container. The identity is the pod's service account, as on
Argo.

```yaml
apiVersion: tekton.dev/v1
kind: Task
metadata:
  name: meridian
spec:
  params:
    - name: command
      default: apply
  workspaces:
    - name: source
    - name: record                    # a PersistentVolumeClaim, shared by every run
  stepTemplate:
    image: ghcr.io/zanattamichael/meridian:0.1.0
    workingDir: $(workspaces.source.path)
    env:
      - name: MERIDIAN_PLUGIN_SIGNATURES
        value: require
      - name: MERIDIAN_STATE_DIR
        value: $(workspaces.record.path)
      - name: SITE_DB_PASSWORD
        valueFrom:
          secretKeyRef: { name: site, key: db-password }
  steps:
    - name: pin
      script: meridian module install --locked
    - name: check
      script: |
        meridian validate site.yaml
        meridian secret check site.yaml
    - name: run
      script: |
        mkdir -p events
        meridian $(params.command) site.yaml --events json > events/run.jsonl
```

A `PipelineRun` chains a `plan` run of the task, a manual approval (a
`custom task`, or a pipeline that stops and is started again), and an
`apply` run; the second `apply` after a failure passes
`command: apply --resume`.

## Octopus Deploy

Reviewed, not executed. A script step in a deployment process. Octopus holds
the environments, the targets, the sensitive variables and the approval
(a manual intervention step before this one), and runs the script on a
worker or a target. The environment variable `OCTOPUS_*` is not one
`meridian` detects, so set `MERIDIAN_ANNOTATIONS=off` or leave the default,
which writes nothing for the engine; the markdown report is attached as an
artifact with Octopus's own service message.

```bash
#!/bin/bash
set -euo pipefail
export MERIDIAN_VERSION='0.1.0' MERIDIAN_VERIFY=cosign MERIDIAN_PLUGIN_SIGNATURES=require
curl -fsSL "https://github.com/ZanattaMichael/meridian-core/releases/download/v$MERIDIAN_VERSION/install.sh" | sh
export PATH="$HOME/.meridian/bin:$PATH"

# Octopus variables arrive through get_octopusvariable; a sensitive one is a
# secret in Meridian's sense only once it is in the job's environment.
export SITE_DB_PASSWORD="$(get_octopusvariable 'Site.DbPassword')"

meridian module install --locked
meridian validate site.yaml
meridian secret check site.yaml
mkdir -p events
meridian apply site.yaml \
    --backend dir --backend-config path=/mnt/meridian/record \
    --events json --report markdown=meridian-apply.md > events/apply.jsonl

new_octopusartifact "$PWD/meridian-apply.md" "meridian-apply.md"
new_octopusartifact "$PWD/events/apply.jsonl" "events.jsonl"
```

## What `meridian` does inside an engine

Every engine sets one well-known variable in the jobs it runs. When
`meridian` finds one, and its standard output is not a terminal, it writes
for the engine as well as for the person reading the log:

| Engine | Detected by | Annotations | Summary |
|---|---|---|---|
| GitHub Actions | `GITHUB_ACTIONS` | `::error file=…,line=…::` workflow commands, shown on the pull request at the line | appended to `$GITHUB_STEP_SUMMARY` |
| Azure Pipelines | `TF_BUILD` | `##vso[task.logissue type=error;sourcepath=…;linenumber=…]` | a file in `$AGENT_TEMPDIRECTORY`, named with `##vso[task.uploadsummary]` |
| Buildkite | `BUILDKITE` | none in the log | `buildkite-agent annotate --context meridian`, when the agent is on the `PATH` |
| GitLab CI | `GITLAB_CI` | none: the engine has no syntax for one | none: use `--report junit=` and the artifacts |
| Jenkins | `JENKINS_URL` | none | none: use `--report junit=` and `--report html=` |
| Argo Workflows | any `ARGO_*` | none | none: keep the events as an output artifact |
| Tekton | any `TEKTON_*` | none | none: keep the events on a workspace |

What is written comes from two places only. An annotation is a document's
diagnostic, as `validate` reports it, or a step of the run's report, which is
folded from the run's [events](../reference/state.md#events); the summary is
the Markdown report that `--report markdown=FILE` writes, followed by where
the record is kept. There is no third place that formats an error, so the
annotation and the log never disagree
([decision 0033](../decisions/0033-annotations-come-from-events-and-the-report-only.md)).
An event never holds a value, so neither does an annotation or a summary.

Annotations go to standard error, beside the error for a person; the
engines read their commands from either stream, and standard output stays
the command's result, so `--events json` and `--output json` are unaffected.
Output is never coloured or animated, inside an engine or out of it.

`--annotations off`, or `MERIDIAN_ANNOTATIONS=off`, turns all of this off.
`--annotations on` keeps it on when standard output is a terminal, which is
how Buildkite runs a job. The option is in
[the command reference](../reference/cli.md#inside-a-pipeline-engine).

## Checking the job's identity first

A missing or wrong identity shows up late, from the tool, after the plan has
started. [`meridian secret check`](../reference/cli.md#secret-check) looks
for every credential declared `from: ambient` where the tool would look,
before anything runs, and reads no value:

```
$ meridian secret check site.yaml
site (site.yaml)
  db_password  env    ok: the environment variable SITE_DB_PASSWORD is set
  deployer     aws    ambient, ok: the environment holds AWS_WEB_IDENTITY_TOKEN_FILE and AWS_ROLE_ARN (web identity, such as a pipeline's OIDC token)
```

| Type | Looked for, in order |
|---|---|
| `aws` | `AWS_ACCESS_KEY_ID` with `AWS_SECRET_ACCESS_KEY`; `AWS_WEB_IDENTITY_TOKEN_FILE` with `AWS_ROLE_ARN`; container credentials; the credentials and config files (`AWS_PROFILE`); the instance metadata service (`AWS_EC2_METADATA_SERVICE_ENDPOINT`, unless `AWS_EC2_METADATA_DISABLED`), asked whether the instance has a role |
| `azure` | `AZURE_CLIENT_ID` and `AZURE_TENANT_ID` with `AZURE_CLIENT_SECRET`, `AZURE_FEDERATED_TOKEN_FILE` or `AZURE_CLIENT_CERTIFICATE_PATH`; the managed identity endpoint (`IDENTITY_ENDPOINT`); the Azure CLI's login (`AZURE_CONFIG_DIR`); the instance metadata service (`AZURE_IMDS_ENDPOINT`), asked whether the machine has a managed identity |
| `gcp` | `GOOGLE_APPLICATION_CREDENTIALS`; `CLOUDSDK_AUTH_ACCESS_TOKEN`; gcloud's application default credentials file (`CLOUDSDK_CONFIG`); the metadata server (`GCE_METADATA_HOST`), asked whether the instance has a service account |
| `ssh` | the agent `SSH_AUTH_SOCK` names |
| `kubeconfig` | the files `KUBECONFIG` names, or `~/.kube/config` |
| any other | not checked: the tool finds it in its own way |

A metadata service is asked one question with a one-second timeout, and
only when nothing in the environment or the files answered first. A
credential that would not be found is `MISSING`, fails the check, and is
annotated like any other problem. Run the check as the first step of every
job, as the snippets do.

## Sharing the record between jobs

Meridian's record (the lock, the run-state and the runs) is what makes a run
resumable and a plan comparable with the last apply. A job's runner is gone
when the job ends, so the record must be somewhere the next job can reach:

- **Carry the state directory as an artifact**, as the GitHub, Azure, GitLab
  and Buildkite snippets do: the plan job keeps `.meridian/`, the apply job
  takes it, a resume takes the failed run's. It is simple and it is enough
  while one pipeline owns the documents and runs one job at a time (every
  snippet has a concurrency group for that). It carries no secret: the
  record names things and never holds a value. The state directory also
  holds each step's [tool output](../reference/state.md#runsidlogs), masked
  as the job's log is, so a failed step can be read with `meridian logs`
  after the runner is gone. To leave it out of the artifact, exclude
  `.meridian/runs/*/logs/` or run with `--no-logs`.
- **A shared backend**, `--backend NAME`, keeps the record in a store every
  runner reaches, and holds the lock there, so two pipelines, or a pipeline
  and a laptop, cannot apply the same documents at once. A backend is a
  small program named `meridian-backend-<name>`;
  [`meridian-backend-dir`](https://github.com/ZanattaMichael/meridian-core/tree/main/examples/backends)
  keeps the record in a directory such as a mounted share, and
  [the contract](../reference/state.md#writing-a-backend) is short enough to
  write one around your object store's CLI in an afternoon. No backend for
  an object store ships yet.

Each tool's own state (a Terraform backend, a Pulumi backend) is the tool's,
set in the document, and is not part of Meridian's record.
[Sharing the record](../concepts/run-safety.md#sharing-the-record) has the
whole picture.

## Approvals

An approval belongs to the engine: its environment, its reviewers, its
audit trail, its pause before production. `meridian apply` asks no question
and needs no `--auto-approve`; the engine's gate stands in front of the job
that runs it, as every snippet above shows (`environment: production`, a
`deployment` job, a protected environment, a `block` step, an `input`, a
`suspend`, a manual intervention). Letting a document name the gate it
expects the engine to hold is the subject of
[issue 185](https://github.com/ZanattaMichael/meridian-core/issues/185),
and generating the pipeline file from the workflow is
[issue 186](https://github.com/ZanattaMichael/meridian-core/issues/186);
neither is needed to run Meridian in a pipeline today.

What the reviewers approved and what the job applies are tied together by a
[saved plan](pull-requests.md): the plan job writes `plan --out`, and the
job behind the gate runs `apply --plan`, which refuses if anything has
changed since.
