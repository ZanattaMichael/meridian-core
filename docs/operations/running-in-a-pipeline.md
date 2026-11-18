# Running Meridian in a pipeline

<p align="center"><img src="../graphics/meridian-in-a-pipeline.svg" alt="Meridian inside a pipeline engine: the engine's band (trigger, runner and identity, secret release, approval, logs and artifacts) above Meridian's band (workflow, orchestrator, target plugins, secret delivery); the engine's job runs meridian apply, and Meridian's events come back as annotations and a job summary" width="100%"></p>

Any engine can run Meridian as one job running `meridian apply`, with no plugin.

The GitHub Actions snippet runs in CI
([`pipeline-shell.yml`](https://github.com/ZanattaMichael/meridian-core/blob/main/.github/workflows/pipeline-shell.yml)),
and a test keeps it in line with this page. **The other snippets are reviewed but not
executed.** For pull request plans, see [saved plans](pull-requests.md).

## Who owns what

The engine decides *when and as whom* and keeps artifacts. Meridian decides *what*, in what
order, and keeps the record.

| The engine owns | Meridian owns |
|---|---|
| **Triggers**: a push, a pull request, a schedule, a button | **Order**: Infrastructure before ResourceSets, imports before the documents that read them, one run at a time on the [lock](../concepts/run-safety.md#one-run-at-a-time) |
| **Runners and their identity**: where the job runs, and the OIDC token or service connection it runs as | **Secrets delivery**: a secret reaches the tool by a [private channel](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) and is never on a command line, in a file the engine keeps, or in a log |
| **Secret release**: which secrets a job may read, and when | **The record**: every run's [events](../reference/state.md#events) and run-state, in a backend the next job can reach |
| **Environments, reviewers and approvals**: who may deploy to production, and the pause before it | **Resume and rollback**: `apply --resume` continues a failed apply from the step that failed; `--on-failure rollback` and `meridian rollback` put one back |
| **Logs and artifacts**: the job's log, its summary page, and the files it keeps | **Drift and policy**: `meridian drift` says whether what was applied is still there; `--policy` asks a policy before anything is applied |

The engine never sees a value Meridian holds, and none is printed
([how](../concepts/secrets-in-memory.md)).

## The shape of every job

Every snippet:

1. **Installs a pinned release** with the [install script](../quickstart/install.md#linux-and-macos)
   (`MERIDIAN_VERSION`, `MERIDIAN_VERIFY=cosign`). Releases start only
   [signed plugins](../reference/plugin-signatures.md); `MERIDIAN_PLUGIN_SIGNATURES=require`
   enforces it for source builds.
2. **Pins modules** with `meridian module install --locked`.
3. **Checks first** with `meridian validate` and `meridian secret check`.
4. **Plans on pull requests, applies on the protected branch.** `--detailed-exitcode`
   exits `2` when there are changes.
5. **Shares the record** ([how](#sharing-the-record-between-jobs)).
6. **Keeps the events** with `--events json` (stdout; the report goes to stderr).
   `meridian report --from FILE` rebuilds a report.

Secrets come from the job environment through the
[`env` provider](../reference/dsl/secrets.md#the-providers), removed from Meridian's
environment once read. The engine's identity is
[`from: ambient`](../reference/dsl/credentials.md#from-ambient).

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

Executed in CI. `id-token: write` enables OIDC; `aws-actions/configure-aws-credentials`,
`azure/login` and `google-github-actions/auth` leave a sign-in `from: ambient` finds. The
state directory passes as an artifact; `concurrency` prevents races.

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

Refusals and failed steps appear as annotations, the report as the step summary. After a
failure, rerun with `resume` ticked to continue from the failed step.

## Azure Pipelines

Not executed. A [workload identity federation service
connection](https://learn.microsoft.com/azure/devops/pipelines/library/connect-to-azure)
and `AzureCLI@2` sign in for `from: ambient`. The record is a pipeline artifact.

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

Problems become `##vso[task.logissue]` lines; the report a `task.uploadsummary` tab. To
resume, restore the record and run `meridian apply site.yaml --resume`.

## GitLab CI

Not executed. `id_tokens` provides OIDC. GitLab has no annotations; the JUnit report shows
steps as tests and the `markdown` report is an artifact.

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

`.oidc-token` is GitLab's token. `SITE_DB_PASSWORD` is a masked CI variable. Use a
[shared backend](#sharing-the-record-between-jobs) when several pipelines share documents.

## Buildkite

Not executed. Buildkite uses a pseudo-terminal, so set `MERIDIAN_ANNOTATIONS=on`. The
report goes through `buildkite-agent annotate`; OIDC from `buildkite-agent oidc
request-token`.

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

Not executed. Secrets from the credentials store; the record is stashed between stages.
Detected by `JENKINS_URL`. Use the JUnit and `html` reports.

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

`from: ambient` finds the agent's identity; `meridian secret check` says which.

## Argo Workflows

Not executed. Steps run the release image; the record is on a shared volume. Detected by
`ARGO_*`. Identity is the pod's workload identity (IRSA, Azure, GKE).

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

Not executed. One task, a workspace for the record. Detected by `TEKTON_*`. Identity as on
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

A `PipelineRun` chains `plan`, an approval and `apply`; resume with
`command: apply --resume`.

## Octopus Deploy

Not executed. A script step after a manual intervention. `OCTOPUS_*` is not detected;
attach the markdown report as an artifact.

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

When an engine's variable is set and stdout is not a terminal, `meridian` also writes:

| Engine | Detected by | Annotations | Summary |
|---|---|---|---|
| GitHub Actions | `GITHUB_ACTIONS` | `::error file=…,line=…::` workflow commands, shown on the pull request at the line | appended to `$GITHUB_STEP_SUMMARY` |
| Azure Pipelines | `TF_BUILD` | `##vso[task.logissue type=error;sourcepath=…;linenumber=…]` | a file in `$AGENT_TEMPDIRECTORY`, named with `##vso[task.uploadsummary]` |
| Buildkite | `BUILDKITE` | none in the log | `buildkite-agent annotate --context meridian`, when the agent is on the `PATH` |
| GitLab CI | `GITLAB_CI` | none: the engine has no syntax for one | none: use `--report junit=` and the artifacts |
| Jenkins | `JENKINS_URL` | none | none: use `--report junit=` and `--report html=` |
| Argo Workflows | any `ARGO_*` | none | none: keep the events as an output artifact |
| Tekton | any `TEKTON_*` | none | none: keep the events on a workspace |

Annotations come only from diagnostics and the report's steps (built from
[events](../reference/state.md#events)); the summary is the `--report markdown` output
([decision 0033](../decisions/0033-annotations-come-from-events-and-the-report-only.md)).
Neither holds values. Annotations go to stderr; stdout is unchanged. No colour.

`--annotations off|on` (or `MERIDIAN_ANNOTATIONS`) overrides detection
([reference](../reference/cli.md#inside-a-pipeline-engine)).

## Checking the job's identity first

[`meridian secret check`](../reference/cli.md#secret-check) looks for each `from: ambient`
credential where the tool would, without reading a value:

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

A metadata service is asked last, with a one-second timeout. `MISSING` fails the check.

## Sharing the record between jobs

The record (lock, run-state, runs) must reach the next job:

- **As an artifact** (`.meridian/`), enough for one pipeline running one job at a time. It
  holds no secret.
- **A shared backend** (`--backend NAME`, a program `meridian-backend-<name>`) locks across
  pipelines and laptops.
  [`meridian-backend-dir`](https://github.com/ZanattaMichael/meridian-core/tree/main/examples/backends)
  uses a directory; see [the contract](../reference/state.md#writing-a-backend). No
  object-store backend ships yet.

Tool state (Terraform or Pulumi backends) is separate. See
[Sharing the record](../concepts/run-safety.md#sharing-the-record).

## Approvals

Approvals are the engine's gates. `meridian apply` asks nothing and needs no
`--auto-approve`. Planned:
[issue 185](https://github.com/ZanattaMichael/meridian-core/issues/185) (documents naming
their gate) and [issue 186](https://github.com/ZanattaMichael/meridian-core/issues/186)
(generating pipeline files).

A [saved plan](pull-requests.md) ties the approved plan to the apply.
