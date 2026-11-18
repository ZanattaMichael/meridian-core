# Target: `cloudformation`

<img src="../branding/logos/meridian-cloudformation.svg" width="72" alt="Meridian CloudFormation plugin logo">

Compiles an Infrastructure document into an AWS CloudFormation template and
deploys it as a stack with the `aws` command line. CloudFormation still does
the work: Meridian writes the template a person would have written, and every
plan and apply goes through a change set, so what is applied is what was
previewed.

## What it does and does not support

| | |
|---|---|
| Category | Infrastructure as code |
| Resource types | CloudFormation's own: any `Vendor::Service::Type` or `Custom::Name`, plus `stack` for the deployment's settings |
| `dependsOn` | Yes, as `DependsOn`, unless a reference already implies the order |
| `notifies` | No. CloudFormation has no way to react to a change in another resource. Refused |
| `runtimeWhen` | No. A CloudFormation condition is decided from the template's parameters, not at deploy time. Refused |
| `state: absent` | No. Remove the resource from the document; CloudFormation deletes what the template no longer declares, subject to its `DeletionPolicy` |
| References | `${id}` is `Ref`, `${id.Attribute}` is `Fn::GetAtt`, inside longer text `Fn::Sub`; see [References](#references) |
| Plan | Yes: a change set, described and then discarded |
| Apply | Yes: the same change set, executed |
| Outputs | Yes: the template's `Outputs`, read back from the stack |
| Editor schemas | Yes: `meridian schema sync` reads the resource provider schemas CloudFormation publishes for the stack's region, with no account and no `aws`. See [Editor schemas](#editor-schemas) |
| Compiles to | `template.yaml` and `stack.json` |
| Runs with | `aws cloudformation` |

What it does not do:

- **No template parameters, mappings or conditions.** Values come from the
  document, through Meridian's own `--data`, `--set` and data layers, and are
  written into the template settled. The compiled template takes no
  parameters.
- **No nested stacks or StackSets.** A document is one stack in one region.
- **No per-type checking at compile time.** The compile checks the shape of
  the document; CloudFormation checks the properties when the change set is
  created, and a template it rejects fails the plan with its reason.
- **It does not delete a stack.** An empty document is refused; delete a stack
  with `aws cloudformation delete-stack` yourself.

Contents: [Requirements](#requirements) ·
[A first document](#a-first-document) ·
[Resources and the stack](#resources-and-the-stack) ·
[References](#references) ·
[Ordering](#ordering) ·
[What a compile writes](#what-a-compile-writes) ·
[Editor schemas](#editor-schemas) ·
[Running](#running) ·
[Settings](#settings) ·
[What is refused](#what-is-refused) ·
[Troubleshooting](#troubleshooting) ·
[How it is tested](#how-it-is-tested)

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | the `aws` command line (version 2), signed in: `aws configure`, a profile, or the environment variables the command reads |
| In AWS | permission to create, describe, execute and delete change sets and to describe the stack, and whatever the stack's resources need. A stack that creates IAM resources must acknowledge `CAPABILITY_IAM` or `CAPABILITY_NAMED_IAM` in its `stack` resource |
| To compile only | Nothing. `meridian compile` never runs `aws` |

## A first document

[`examples/cloudformation/web-vm.yaml`](../../examples/cloudformation/web-vm.yaml)
deploys a virtual machine with a fixed public address, a security group and
an instance role. The part that shows what is particular to CloudFormation:

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web-vm
spec:
  target: cloudformation
  resources:
    # Not part of the template: how the stack itself is deployed.
    - id: deployment
      type: stack
      params:
        name: web-vm-prod
        region: us-east-1
        capabilities: [CAPABILITY_NAMED_IAM]

    - id: web_sg
      type: AWS::EC2::SecurityGroup
      params:
        GroupDescription: Web traffic for ${AWS::StackName}
        SecurityGroupIngress:
          - IpProtocol: tcp
            FromPort: 80
            ToPort: 80
            CidrIp: 0.0.0.0/0

    - id: web_vm
      type: AWS::EC2::Instance
      params:
        ImageId: ami-0c7217cdde317cfec
        InstanceType: t3.small
        SecurityGroupIds: ["${web_sg.GroupId}"]
        DeletionPolicy: Retain
      # Nothing in the properties says so, so this becomes DependsOn.
      dependsOn:
        - resource: web_role

    - id: web_ip
      type: AWS::EC2::EIP
      params:
        InstanceId: ${web_vm}

  outputs:
    - name: web_vm_ip
      value: ${web_ip.PublicIp}
```

It compiles to
([the whole template](../../plugins/cloudformation/testdata/web-vm.template_yaml)):

```yaml
AWSTemplateFormatVersion: "2010-09-09"
Resources:
  WebSg:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription:
        Fn::Sub: Web traffic for ${AWS::StackName}
      SecurityGroupIngress:
        - CidrIp: 0.0.0.0/0
          FromPort: 80
          IpProtocol: tcp
          ToPort: 80
  WebVm:
    Type: AWS::EC2::Instance
    DependsOn:
      - WebRole
    DeletionPolicy: Retain
    Properties:
      ImageId: ami-0c7217cdde317cfec
      InstanceType: t3.small
      SecurityGroupIds:
        - Fn::GetAtt:
            - WebSg
            - GroupId
  WebIp:
    Type: AWS::EC2::EIP
    Properties:
      InstanceId:
        Ref: WebVm
Outputs:
  WebVmIp:
    Value:
      Fn::GetAtt:
        - WebIp
        - PublicIp
```

## Resources and the stack

A resource's `type` is the CloudFormation resource type and its `params` are
that type's properties, exactly as AWS documents them, in AWS's own case.
Meridian does not know the types: it writes what the document says and lets
CloudFormation check it.

A few keys are resource attributes and not properties. Written among the
params, they are placed beside `Properties` in the template:
`DeletionPolicy`, `UpdateReplacePolicy`, `CreationPolicy`, `UpdatePolicy`,
`Condition` and `Metadata`.

One resource may have the type `stack`. It is not part of the template. It
holds how the stack is deployed:

| Param | Meaning | Default |
|---|---|---|
| `name` | the stack's name: a letter, then letters, digits and hyphens | the document's name |
| `region` | the region to deploy to | the `aws` command's own default |
| `capabilities` | the capabilities the deployment acknowledges: `CAPABILITY_IAM`, `CAPABILITY_NAMED_IAM`, `CAPABILITY_AUTO_EXPAND` | none |

A second `stack` resource is refused (`duplicate-resource`), and a `stack` may
not take part in `dependsOn` (`unsupported-dependency`).

Logical ids hold only letters and digits, so a resource id such as `web_vm`
is written `WebVm` in the template, and an output `web_vm_ip` is `WebVmIp`.
Two ids that become the same logical id are refused. Everything Meridian
reports uses the id you wrote.

## References

| You write | It becomes |
|---|---|
| `${web_vm}` | `Ref: WebVm` |
| `${web_vm.PublicIp}` | `Fn::GetAtt: [WebVm, PublicIp]` |
| either inside longer text | `Fn::Sub`, with the reference rewritten to its logical id |
| `${AWS::Region}`, `${AWS::StackName}` and the other pseudo parameters | passed through inside `Fn::Sub` |
| `$${` | a literal `${` |

Anything else between `${` and `}` is refused (`invalid-reference`): a
reference must name a resource of the document, optionally with one
attribute, or be a pseudo parameter. Values from `--data`, `--set` and data
layers are settled before the template is written and never become
references
([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions)).

## Ordering

A `Ref` or `Fn::GetAtt` already orders two resources, so a `dependsOn` that
a reference implies is left out of the template. Only an order nothing in the
properties says becomes `DependsOn`, as in the example above. The resources
are written in the order Meridian decided, which CloudFormation does not
need but a reader does.

## What a compile writes

```
template.yaml
stack.json
```

`template.yaml` is the CloudFormation template: `Resources`, with the
properties' keys sorted, and, when the document has outputs, `Outputs`. It is an ordinary template, so you can
deploy it with the console or a pipeline of your own. `stack.json` holds what
the runner needs and the template does not say: the stack's name, region and
capabilities, and the map from logical ids back to the ids you wrote, so a
change set's rows are reported by your names.

## Editor schemas

`meridian schema sync` reads the registry of resource types CloudFormation
publishes for a region, so an editor completes and checks `AWS::S3::Bucket`'s
properties as it does a plugin's own types
([Tool schemas](../tooling/schema-sync.md)). The registry is one zip per
region of resource provider schemas, the JSON Schema CloudFormation itself
validates a template against, at
`https://schema.cloudformation.<region>.amazonaws.com/CloudformationSchema.zip`.
No account and no `aws` command is needed to read it. The region is the
`stack` resource's `region`, else `--region` in `MERIDIAN_AWS_ARGS`, else
`AWS_REGION` or `AWS_DEFAULT_REGION`, else `us-east-1`.

| | |
|---|---|
| Described in full | the `Vendor::Service::Type` types the documents use: every property with its kind, whether it is required, the values it allows and its nested properties; the `readOnlyProperties` as set by CloudFormation (read with `${id.Arn}`, never written); the `createOnlyProperties` marked as replacing the resource when changed; the `writeOnlyProperties` as sensitive |
| Listed by name | every other type of the region's registry, with the first paragraph of its description |
| Pinned | the region, to the SHA-256 of the zip it was read from |

A type that is not in the public registry, a private or third-party type
the account activated, is asked of `aws cloudformation describe-type`, which
needs the command and credentials; a type neither has is a note. The zip is
kept in the work directory and fetched again only when the registry says it
changed; when the registry cannot be reached, the last copy is used and the
sync says so. The source is tested against the real registry in the
`cloudformation` workflow.

## Running

Both plan and apply go through a change set, so what is applied is what was
previewed. A change set's name is `meridian-<time>`.

| | |
|---|---|
| Plan | `describe-stacks` to learn whether the stack exists, `create-change-set` (`CREATE` for a new stack, `UPDATE` otherwise), `wait change-set-create-complete`, `describe-change-set`, then `delete-change-set`. For a stack that does not exist yet, the empty stack in review that creating the change set left behind is deleted too |
| Apply | the same preview, then `execute-change-set`, `wait stack-create-complete` or `stack-update-complete`, and `describe-stacks` for the outputs. A preview with nothing to change is discarded and nothing is executed |
| Saved plan | `meridian plan --out` keeps the change set instead of discarding it and records its name; `meridian apply --plan` executes that change set, so what is applied is what the reviewer read. The change set's values stay with CloudFormation; the plan file holds only its name ([a saved plan](../operations/pull-requests.md)) |
| Outputs later | `describe-stacks` |

Changes are read from the change set's description, resource by resource,
with the action CloudFormation says: `Add` is a create, `Remove` a delete,
and `Modify` an update, or a replace when CloudFormation says the resource
will be replaced. Each is reported under the id you wrote. A change set CloudFormation marks `FAILED` because the
template holds no changes is a plan of nothing to do; one it marks `FAILED`
for any other reason fails the stage with CloudFormation's own words, and is
deleted.

A template CloudFormation rejects fails the plan when the change set is
created, with the reason, and leaves no stack behind. A deployment that fails
part way is rolled back by CloudFormation itself; the stage fails and says
to read the stack's events for which resource failed.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) in a resource's
params reaches CloudFormation as a template parameter marked `NoEcho`, whose value
is read from a private file, and through nothing else.

```yaml
spec:
  target: cloudformation
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db_password_parameter
      type: AWS::SSM::Parameter
      params:
        Name: /app/db-password
        Type: String
        Value: ${{ secret.db_password }}
```

**What the compile writes.** The param becomes `Ref: MeridianSecretDbPassword`, or
`${MeridianSecretDbPassword}` inside an `Fn::Sub` when the secret sits in longer
text, and the template declares the parameter:

```yaml
Parameters:
  MeridianSecretDbPassword:
    Type: String
    NoEcho: true
```

A parameter's name is letters and digits only, so the secret's name is spelled the
way a resource id is spelled as a logical id: `db_password` is
`MeridianSecretDbPassword`. Two secrets, or a secret and a resource, that come down to
the same name are refused (`invalid-name`) rather than merged. The compiled template
holds the parameter's name and never its value.

**How the value reaches the tool.** When a stage runs, the runner writes every secret
into one parameters file, `[{"ParameterKey": ..., "ParameterValue": ...}]`, in a
directory only the user can enter (mode `0700`, the file `0600`, on a memory
filesystem where the system has one), and passes it as `--parameters file://<path>`
to `create-change-set`. The file is overwritten and removed as soon as the change set
exists. The change set carries the values, and CloudFormation shows `****` for a
`NoEcho` parameter wherever it describes the stack or change set. The command
line names only the file's path, and `aws`'s environment holds nothing of the
value.

**What CloudFormation keeps.** The stack keeps the parameter's value, masked in every
description, as it keeps every parameter. A resource property given the value is kept
by the service that owns the resource, the way that service keeps it: a
`AWS::SSM::Parameter` of type `String` is readable by anyone who can read parameters.
Choose a type or a resource that protects what it holds.

**What is refused.** A secret in the `stack` resource's settings is refused at compile,
rule `unsupported-secret-place`: the settings are the command line `aws` is given.

The handover is tested two ways:

- against a stand-in `aws` that records the parameters file, its mode, its
  arguments and its environment (`plugins/cloudformation/tests/run.rs`)
- against the real `aws` and LocalStack in CI
  (`plugins/cloudformation/tests/real.rs`): the value reaches a Systems Manager
  parameter and is not in the plugin's output, its files or the stack's
  description

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_AWS` | the program to run, when it is not `aws` on `PATH` |
| `MERIDIAN_AWS_ARGS` | arguments added to every call, split like a shell command line (quotes hold a word together): `--profile prod`, or `--endpoint-url http://localhost:4566` for an emulator |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.cloudformation`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#cloudformation).

Credentials, and the default region when the `stack` resource names none,
are the `aws` command's own.

## What is refused

Every refusal names the resource, the line and the rule. The rules this
target can raise, and what to do:

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | a type that is not `Vendor::Service::Type`, `Custom::Name` or `stack` | use the CloudFormation type as AWS writes it |
| `unsupported-state` | `state: absent`, or any state but `present` | remove the resource from the document instead |
| `unsupported-runtime-condition` | `runtimeWhen` on any resource | decide with a compile-time `when` instead |
| `unsupported-action` | `notifies` on any resource | drop it; CloudFormation has no notifications |
| `unsupported-dependency` | a `dependsOn` to or from the `stack` resource | the stack is not a resource of the template |
| `unsupported-param` | a param the `stack` resource does not take | only `name`, `region` and `capabilities` |
| `invalid-param` | a `stack` param of the wrong kind, or a capability CloudFormation does not know | a string for `name` and `region`, a list of `CAPABILITY_*` strings for `capabilities` |
| `invalid-name` | a stack name that does not start with a letter or holds something other than letters, digits and hyphens | rename it |
| `duplicate-resource` | two resources of type `stack` | keep one |
| `invalid-name` | a resource or output id with no letters or digits to make a logical id from | rename it |
| `duplicate-resource`, `duplicate-output` | two resources, or two outputs, that become the same logical id (`web_vm` and `WebVm`) | rename one |
| `invalid-reference` | `${...}` that is not a resource of the document, one attribute of it, or a pseudo parameter | fix the reference |
| `empty-document` | a document with no resources but the stack | add a resource, or delete the stack yourself |

## Troubleshooting

| What you see | Why | What to do |
|---|---|---|
| `could not run aws: it is not installed, or not on the PATH` | the plugin runs `aws` on the machine running Meridian | install the AWS CLI, or set `MERIDIAN_AWS` |
| `Unable to locate credentials` | the `aws` command is not signed in | `aws configure`, or `MERIDIAN_AWS_ARGS=--profile <name>` |
| `Requires capabilities : [CAPABILITY_IAM]` | the template creates IAM resources | add the capability to the `stack` resource |
| `CloudFormation rejected the change set: ...` | the template is not one CloudFormation accepts; the reason is CloudFormation's | fix the property it names; no stack was left behind |
| the plan says nothing to do but the stack is wrong | a change set compares the template with what CloudFormation recorded, not with the resources themselves | drift is CloudFormation's `detect-stack-drift`; Meridian does not run it |
| `the stack did not reach a complete state` | a resource failed and CloudFormation rolled the deployment back | read the stack's events in the console or with `describe-stack-events` |
| a stack is stuck in `REVIEW_IN_PROGRESS` | a plan was interrupted before it could discard its change set | the next plan reuses it as a new stack; or delete the stack yourself |

## How it is tested

- **Golden files** under `plugins/cloudformation/testdata/` record what the
  example compiles to, `template.yaml` and `stack.json`, byte for byte, and
  what each refusal says.
- **Run tests** (`plugins/cloudformation/tests/run.rs`) drive the runner
  against a stand-in for `aws`: the exact calls in order, a new stack and an
  existing one, a failed change set, the one that holds no changes, and the
  outputs read back.
- **Real-tool tests** (`plugins/cloudformation/tests/real.rs`, workflow
  `cloudformation.yml`) run the real `aws` command against
  [LocalStack](https://github.com/localstack/localstack), an emulator of AWS:
  a stack is previewed, created, read back, changed and trimmed, and a
  template CloudFormation rejects fails the plan and leaves no stack. They are
  marked `#[ignore]` and run with
  `MERIDIAN_TEST_REAL_CLOUDFORMATION=1 cargo test -p meridian-target-cloudformation --test real -- --ignored`,
  with `MERIDIAN_AWS_ARGS` pointing at the emulator. They have not been run
  against a real AWS account.
  [Real-tool tests](../contributing/real-tool-tests.md#cloudformation) says
  what the emulator proves, what it cannot, and how to run the same tests
  against an account.
