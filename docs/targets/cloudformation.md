# Target: `cloudformation`

<img src="../branding/logos/meridian-cloudformation.svg" width="72" alt="Meridian CloudFormation plugin logo">

Compiles an Infrastructure document into a CloudFormation template, deployed as a stack
with `aws` through a change set, so what is applied is what was previewed.

## What it does and does not support

| | |
|---|---|
| Category | Infrastructure as code |
| Resource types | CloudFormation's own: any `Vendor::Service::Type` or `Custom::Name`, plus `stack` for the deployment's settings. Also the [common infrastructure types](#common-infrastructure-types) |
| `dependsOn` | Yes, as `DependsOn`, unless a reference already implies the order |
| `notifies` | No. CloudFormation has no way to react to a change in another resource. Refused |
| `runtimeWhen` | No. A CloudFormation condition is decided from the template's parameters, not at deploy time. Refused |
| `adopt` | No. Refused: CloudFormation adopts only through a resource-import change set, which this target does not make yet |
| `state: absent` | No. Remove the resource from the document; CloudFormation deletes what the template no longer declares, subject to its `DeletionPolicy` |
| References | `${id}` is `Ref`, `${id.Attribute}` is `Fn::GetAtt`, inside longer text `Fn::Sub`; see [References](#references) |
| Plan | Yes: a change set, described and then discarded |
| Apply | Yes: the same change set, executed |
| Outputs | Yes: the template's `Outputs`, read back from the stack |
| Destroy | Yes: the stack is deleted. See [Destroying](../operations/destroying.md) |
| Editor schemas | Yes: `meridian schema sync` reads the resource provider schemas CloudFormation publishes for the stack's region, with no account and no `aws`. See [Editor schemas](#editor-schemas) |
| Compiles to | `template.yaml` and `stack.json` |
| Runs with | `aws cloudformation` |

Not supported:

- **Template parameters, mappings and conditions.** Values are settled from `--data`,
  `--set` and data layers.
- **Nested stacks and StackSets.** One stack, one region.
- **Per-type checks at compile.** CloudFormation checks properties when the change set is
  created.
- **Deleting a stack.** Use `aws cloudformation delete-stack`.

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | the `aws` command line (version 2), signed in: `aws configure`, a profile, or the environment variables the command reads |
| In AWS | permission to create, describe, execute and delete change sets and to describe the stack, and whatever the stack's resources need. A stack that creates IAM resources must acknowledge `CAPABILITY_IAM` or `CAPABILITY_NAMED_IAM` in its `stack` resource |
| To compile only | Nothing. `meridian compile` never runs `aws` |

## A first document

[`examples/cloudformation/web-vm.yaml`](../../examples/cloudformation/web-vm.yaml) deploys
a VM with an address, a security group and an instance role:

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

`type` is the CloudFormation type; `params` are its properties, in AWS's case.
CloudFormation checks them.

`DeletionPolicy`, `UpdateReplacePolicy`, `CreationPolicy`, `UpdatePolicy`, `Condition` and
`Metadata` are placed beside `Properties`.

An optional `stack` resource sets how the stack is deployed:

| Param | Meaning | Default |
|---|---|---|
| `name` | the stack's name: a letter, then letters, digits and hyphens | the document's name |
| `region` | the region to deploy to | the `aws` command's own default |
| `capabilities` | the capabilities the deployment acknowledges: `CAPABILITY_IAM`, `CAPABILITY_NAMED_IAM`, `CAPABILITY_AUTO_EXPAND` | none |

A second is refused (`duplicate-resource`), as is `dependsOn` on it
(`unsupported-dependency`).

Ids become logical ids: `web_vm` is `WebVm`. Collisions are refused. Reports use your ids.

## References

| You write | It becomes |
|---|---|
| `${web_vm}` | `Ref: WebVm` |
| `${web_vm.PublicIp}` | `Fn::GetAtt: [WebVm, PublicIp]` |
| either inside longer text | `Fn::Sub`, with the reference rewritten to its logical id |
| `${AWS::Region}`, `${AWS::StackName}` and the other pseudo parameters | passed through inside `Fn::Sub` |
| `$${` | a literal `${` |

Anything else in `${}` is refused (`invalid-reference`). Values from data never become
references ([see](../reference/dsl/interpolation.md#a-tools-own-expressions)).

## Common infrastructure types

`network`, `subnet`, `firewall`, `address`, `vm`, `disk`, `objectStore` and `dnsRecord` are Meridian's own types,
and this target turns each into CloudFormation's own
([the common infrastructure types](../reference/dsl/common-infrastructure-types.md)
has every param and mapping). They are created in the stack's region, and
mapped as the Terraform target maps them on AWS, with the same sizes,
refusals and outputs.

| Common type | Becomes |
|---|---|
| `network` | `AWS::EC2::VPC`, with DNS host names on |
| `subnet` | `AWS::EC2::Subnet`. With `public: true`, also an `AWS::EC2::InternetGateway` and `AWS::EC2::VPCGatewayAttachment` per network, a route table, a route to the internet that waits for the attachment, and the table's association |
| `firewall` | `AWS::EC2::SecurityGroup`, the rules inside it |
| `address` | `AWS::EC2::EIP` |
| `vm` | `AWS::EC2::Instance`, an `AWS::EC2::KeyPair` for `sshKey`, an `AWS::EC2::EIPAssociation` for `address` |
| `disk` | `AWS::EC2::Volume` and an `AWS::EC2::VolumeAttachment` |
| `objectStore` | `AWS::S3::Bucket` |
| `dnsRecord` | `AWS::Route53::RecordSet` |

An image given as `{family, version}` is read when the stack is deployed from
the vendor's public Systems Manager parameter, as `{{resolve:ssm:...}}`.
Compare [`examples/common/web-cloudformation.yaml`](../../examples/common/web-cloudformation.yaml)
with [`web-aws.yaml`](../../examples/common/web-aws.yaml).

## Ordering

A `dependsOn` a `Ref` or `Fn::GetAtt` implies is left out; others become `DependsOn`.
Resources are written in Meridian's order.

## What a compile writes

```
template.yaml
stack.json
```

`template.yaml` holds `Resources` (keys sorted) and `Outputs`. `stack.json` holds the
stack's name, region, capabilities and the map back to your ids.

## Editor schemas

`meridian schema sync` reads the region's schema zip,
`https://schema.cloudformation.<region>.amazonaws.com/CloudformationSchema.zip`, with no
account ([Tool schemas](../tooling/schema-sync.md)). Region: the `stack` resource's, then
`--region` in `MERIDIAN_AWS_ARGS`, then `AWS_REGION` or `AWS_DEFAULT_REGION`, then
`us-east-1`.

| | |
|---|---|
| Described in full | the `Vendor::Service::Type` types the documents use: every property with its kind, whether it is required, the values it allows and its nested properties; the `readOnlyProperties` as set by CloudFormation (read with `${id.Arn}`, never written); the `createOnlyProperties` marked as replacing the resource when changed; the `writeOnlyProperties` as sensitive |
| Listed by name | every other type of the region's registry, with the first paragraph of its description |
| Pinned | the region, to the SHA-256 of the zip it was read from |

Private and third-party types use `aws cloudformation describe-type` (needs credentials).
The zip is cached; offline, the cache is used with a note.

## Running

Both use a change set named `meridian-<time>`.

| | |
|---|---|
| Plan | `describe-stacks` to learn whether the stack exists, `create-change-set` (`CREATE` for a new stack, `UPDATE` otherwise), `wait change-set-create-complete`, `describe-change-set`, then `delete-change-set`. For a stack that does not exist yet, the empty stack in review that creating the change set left behind is deleted too |
| Apply | the same preview, then `execute-change-set`, `wait stack-create-complete` or `stack-update-complete`, and `describe-stacks` for the outputs. A preview with nothing to change is discarded and nothing is executed |
| Saved plan | `meridian plan --out` keeps the change set instead of discarding it and records its name; `meridian apply --plan` executes that change set, so what is applied is what the reviewer read. The change set's values stay with CloudFormation; the plan file holds only its name ([a saved plan](../operations/pull-requests.md)) |
| Outputs later | `describe-stacks` |
| Destroy | `describe-stacks`, `list-stack-resources`, `delete-stack`, then `wait stack-delete-complete`; `plan --destroy` stops after `list-stack-resources`. No stack: nothing to remove. A resource with `DeletionPolicy: Retain` is left in the account |

`Add` is a create, `Remove` a delete, `Modify` an update or replace, reported by id. A
`FAILED` change set with no changes is nothing to do; any other `FAILED` fails the stage and
is deleted.

A rejected template fails the plan and leaves no stack. A failed deployment is rolled back
by CloudFormation; read the stack's events.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) becomes a `NoEcho`
parameter read from a private file.

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

The param becomes `Ref: MeridianSecretDbPassword` (or `Fn::Sub` in longer text):

```yaml
Parameters:
  MeridianSecretDbPassword:
    Type: String
    NoEcho: true
```

Name collisions are refused (`invalid-name`).

The runner writes a parameters file (mode `0600`, in a `0700` directory, in memory where
possible), passed as `--parameters file://<path>` to `create-change-set` and removed once
the change set exists. CloudFormation shows `****` for it.

The stack keeps the value masked. A property keeps it as its service does: an SSM
`String` parameter is readable. Choose a type that protects it.

A secret in `stack` settings is refused (`unsupported-secret-place`).

Tested against a stand-in `aws` (`plugins/cloudformation/tests/run.rs`) and LocalStack in CI
(`plugins/cloudformation/tests/real.rs`).

## Credentials

`spec.endpoint` names the [credential](../reference/dsl/credentials.md#reaching-a-cloud-as-a-credential)
the cloud is reached as. The AWS command line reads its login from the environment, as every AWS SDK does, so an `aws` credential reaches each `aws cloudformation` call of the stage there. CloudFormation is AWS only: an `azure` or `gcp` credential is refused at compile.

```yaml
spec:
  target: cloudformation
  endpoint:
    type: cloud
    credential: prod
  secrets:
    - name: aws_secret_key
      from: env
      key: PROD_AWS_SECRET_KEY
  credentials:
    - name: prod
      type: aws
      accessKeyId: AKIAIOSFODNN7EXAMPLE
      secretAccessKey: ${{ secret.aws_secret_key }}
      region: eu-west-1
  resources:
    - id: logs
      type: AWS::S3::Bucket
      params:
        BucketName: shop-logs
```

| Credential | Plain fields, as variables | Secret fields |
|---|---|---|
| `aws` | `accessKeyId` as `AWS_ACCESS_KEY_ID`, `profile` as `AWS_PROFILE`, `region` as `AWS_REGION` and `AWS_DEFAULT_REGION` (the stack's own `region` setting still goes on the command line as `--region`) | `secretAccessKey` as `AWS_SECRET_ACCESS_KEY`, `sessionToken` as `AWS_SESSION_TOKEN` |

- **In the tool's environment, and nowhere else.** The providers take a login from
  the environment, from their own files under your home directory, or from the
  machine's metadata service; Meridian holds the credential in protected memory and
  writes nothing under your home, so the environment is the way: rank 4 of the plan's
  [channels](../reference/dsl/secrets.md#handing-a-secret-to-a-tool), the last it
  allows, taken because the providers take a value no other way. The value goes from
  protected memory into the process's environment as it is spawned, and is copied
  nowhere else by Meridian. It is never on the command line, in the compiled files or
  in a file Meridian keeps. The plan line says so: `as prod (aws, region eu-west-1),
  in the tool's environment`.
- **What that means.** Another process of yours can read a running tool's environment
  (`/proc/<pid>/environ`), and everything the tool starts, such as a provider plugin,
  inherits it. That is the same exposure as exporting the variables in a shell before
  running the tool by hand, and no more.
- **A file where a provider reads one.** A `gcp` `serviceAccountKey` and an `azure`
  `certificate` are written to a file only you can read, held in memory where the
  system allows, and the environment names its path (`GOOGLE_APPLICATION_CREDENTIALS`,
  `ARM_CLIENT_CERTIFICATE_PATH`). The file is removed when the stage ends.
- **`from: ambient`** names the identity without Meridian holding any of it: the
  tool finds the login as it does today. The plain fields are still told, so an
  ambient `aws` credential with a `profile` sets `AWS_PROFILE` for the run.
- **Refused at compile**, rule `unsupported-credential`: a credential of a type the
  tool does not reach a cloud as, an `aws` `roleArn` (the SDKs assume a role through
  a profile that names it; declare that profile), and a plain field with a line break
  in it.

The compiled files hold nothing of the credential, and the plan and the run record
name it (`prod (aws, region eu-west-1)`) and nothing more. Outputs read on their own
(`meridian outputs`) are read with no credential; a backend that needs the cloud's
login to read the state is read right after the apply, inside it.

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_AWS` | the program to run, when it is not `aws` on `PATH` |
| `MERIDIAN_AWS_ARGS` | arguments added to every call, split like a shell command line (quotes hold a word together): `--profile prod`, or `--endpoint-url http://localhost:4566` for an emulator |

Settings can also go in `meridian.yaml` under `spec.settings.cloudformation`; the
environment wins. See the
[plugin settings reference](../reference/plugin-settings.md#cloudformation). Credentials
and the default region are `aws`'s own.

## What is refused

Each names the resource, line and rule:

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

## Reading a failure

When the stack does not reach a complete state, `aws cloudformation describe-stack-events`
is read back to the stack's `User Initiated` event. Each resource with a `*_FAILED`
status is a [resource failure](../reference/state.md#resource-failures): the logical id mapped to the document's id, the
`ResourceStatusReason` as the summary and the status (`CREATE_FAILED`) as the tool code.
A resource CloudFormation cancelled because another failed is `dependency-failed`; one
refused for permissions `auth-failed`; any other `resource-failed`, or `unreachable`, `auth-failed`, `timed-out` or `reboot-required` when the message says so. A destroy
reads them the same way.

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

[`--tool-debug`](../reference/cli.md#asking-a-tool-for-more-output) adds `--debug` to `aws` at every level.

## How it is tested

- **Golden files** in `plugins/cloudformation/testdata/`, refusals included.
- **Run tests** (`plugins/cloudformation/tests/run.rs`) against a stand-in `aws`.
- **Real-tool tests** (`plugins/cloudformation/tests/real.rs`, `cloudformation.yml`) run
  `aws` against [LocalStack](https://github.com/localstack/localstack), with
  `MERIDIAN_TEST_REAL_CLOUDFORMATION=1 cargo test -p meridian-target-cloudformation --test real -- --ignored`
  and `MERIDIAN_AWS_ARGS` pointing at it. Not yet run against real AWS; see
  [Real-tool tests](../contributing/real-tool-tests.md#cloudformation).
