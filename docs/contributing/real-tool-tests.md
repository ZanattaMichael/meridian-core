# Real-tool tests

Every plugin has two kinds of test for the half that runs its tool.

- **Run tests** (`tests/run.rs`) drive the runner against a stand-in: a script that
  prints what the tool is believed to print. They need nothing installed, so they run
  everywhere, and they pin down the exact commands, the environment and the error
  handling.
- **Real-tool tests** (`tests/real.rs`, and a few files with other names) run the tool
  itself. They are the only thing that proves the belief the stand-in encodes.

This page lists the real-tool tests: what each one runs, what that proves, what it cannot
prove, and how to run it yourself.

## How they are switched on

A real-tool test is skipped unless an environment variable asks for it, so
`cargo test --workspace` needs nothing but Rust. Each has a workflow of its own under
`.github/workflows/` that installs the tool, sets the variable and runs the one test
file. The workflows are limited by `paths:` to the plugin and the crates it depends on.

Two rules keep a green job honest:

- **Asked for and missing is a failure.** With the variable set, a tool that is not
  installed, a cluster that cannot be reached or a module that cannot be imported fails
  the test. A job can never pass by checking nothing.
- **The version is on the log.** Each workflow prints the version of the tool it tested,
  and pins it where the tool's installer lets it.

## What each target is tested against

| Target | Workflow | Runs against | Switch |
|---|---|---|---|
| `ansible` | `ansible.yml` | `ansible-playbook` (ansible-core 2.19) over the local connection | `MERIDIAN_TEST_REAL_ANSIBLE=1` |
| `arm` | `azure.yml` | the Bicep compiler reading each template; Azure itself [when a subscription is configured](#azure) | `MERIDIAN_TEST_REAL_ARM=1` |
| `bicep` | `azure.yml` | the Bicep compiler building each file; Azure itself [when a subscription is configured](#azure) | `MERIDIAN_TEST_REAL_BICEP=1` |
| `chef` | `chef.yml` | Cinc Client 18 in local mode, as the user and as root | `MERIDIAN_TEST_REAL_CHEF=1` |
| `cloud-init` | `cloud-init.yml` | `cloud-init schema` | `MERIDIAN_TEST_REAL_CLOUD_INIT=1` |
| `cloudformation` | `cloudformation.yml` | the `aws` command against [LocalStack](#cloudformation) | `MERIDIAN_TEST_REAL_CLOUDFORMATION=1` |
| `docker` | `docker.yml` | the Docker daemon and Compose | `MERIDIAN_REAL_DOCKER=1` |
| `dsc` | `dsc.yml` | PowerShell, DscPipelineRunner and DSC v3 on Linux; the plugin's runner with DSC v2 and DSC v3, and real Windows resources, [on Windows](#dsc) | `MERIDIAN_TEST_REAL_DSC=1` |
| `helm` | `helm.yml` | `helm`, `kustomize` and a kind cluster | `-- --ignored` |
| `kubernetes` | `kubernetes.yml` | `kubectl` and a kind cluster | `MERIDIAN_TEST_REAL_KUBECTL=1` |
| `opentofu` | `opentofu.yml` | `tofu` | `MERIDIAN_TEST_TOFU=tofu` |
| `openvox` | `openvox.yml` | `puppet` from the OpenVox gem | `MERIDIAN_TEST_OPENVOX=1` |
| `packer` | `packer.yml` | `packer` | `-- --ignored` |
| `podman` | `podman.yml` | Podman and systemd | `MERIDIAN_TEST_PODMAN=system` |
| `pulumi` | `pulumi.yml` | `pulumi` with a local backend | `MERIDIAN_REAL_PULUMI=1` |
| `puppet` | none yet | stand-ins only; the `openvox` target shares its compiler and is tested for real | |
| `salt` | `salt.yml` | `salt-call` and `salt-ssh` | `MERIDIAN_TEST_SALT_CALL=<program>` |
| `terraform` | `terraform.yml` | `terraform`, and the same tests again with `tofu` | `MERIDIAN_TEST_REAL_TERRAFORM=1` |

Each target's page says, under its running section, what its real-tool test covers.

## What a real-tool test should do

The ones in this repository follow one shape. A new one should too.

1. Compile a small document with the plugin.
2. **Plan**, and check both what the plan reports and that nothing was created.
3. **Apply**, and check the result where it really lives: the file on disk, the object in
   the cluster, the stack's resources. Do not only check what the plugin reported.
4. **Plan again**, and check it reports nothing to change. A plan that is never in sync
   is the most common thing a stand-in hides.
5. Change the document and check the plan calls the change what it is: an update, a
   replacement, a deletion.
6. Give the tool something it refuses, and check the failure carries the tool's own
   reason.
7. Clean up in a `Drop`, so that a failed assertion leaves nothing behind.

Use resources that need no account and cost nothing: Terraform's `terraform_data`, a
ConfigMap, a file under the test's scratch directory.

When the real tool disagrees with the stand-in, fix the runner, correct the stand-in's
text in `tests/run.rs` so that it matches what the tool really prints, and say what you
found in the pull request.

## Notes on particular targets

### CloudFormation

The tests run the real `aws` command line, pointed at
[LocalStack](https://github.com/localstack/localstack) by
`MERIDIAN_AWS_ARGS='--endpoint-url http://localhost:4566'`. LocalStack is an emulator of
AWS that runs in a container on the runner and creates the queue, topic and parameter the
stack asks for, so the tests need no account and no secret.

What that proves: the template is one CloudFormation accepts, the plugin's sequence of
change-set calls works end to end, and a preview, an apply, a no-op, an update and a
deletion are each reported as what they are.

What it cannot prove: that the real service behaves as the emulator does in every detail,
in particular how long a change set takes and what a replacement looks like. Two places
where the emulator was seen to differ: LocalStack 4.4 reported an unchanged resource as an
update, which 4.14, the version the workflow pins, does not; and LocalStack accepts a
resource type that does not exist, which CloudFormation refuses, so the refusal the tests
use is a reference to a resource the template does not have, which both refuse in the
same words. To run the
same tests against a real account, sign the `aws` command in, leave `MERIDIAN_AWS_ARGS`
unset and set `MERIDIAN_TEST_REAL_CLOUDFORMATION=1`. They create a stack named
`meridian-real` in `us-east-1` and delete it.

### Azure

Azure has no emulator and no way to preview a deployment offline: `what-if` is a call to
Resource Manager. So the Bicep and ARM plugins are tested in two parts.

**Without an account**, on every pull request that touches them:

- Bicep: the real Bicep compiler builds every file the plugin compiles. The compiler
  parses the file, checks each resource against Azure's published resource types and
  produces the ARM template that would be sent. The test also checks that the built
  template holds the resources, names and values the document asked for.
- ARM: nothing validates an ARM template offline the way Resource Manager does. The
  nearest real tool is the Bicep compiler's ARM reader (`az bicep decompile`), which has
  to parse the whole template to convert it: schema and scope, every resource, every
  template expression, every reference to a parameter. Each template the plugin compiles
  is put through it, and a deliberately broken one is checked to be refused. One kind of
  complaint from the reader is allowed: a template names another resource by its id, in
  `dependsOn` and in the `scope` of an extension resource such as a lock, and the
  converter cannot always turn an id into the resource symbol Bicep wants. It has read
  and written out the whole template by then. Whether Azure accepts those ids is for a
  subscription to prove.

That proves what Meridian writes is valid. **It does not prove anything about the
runner**: `az deployment group what-if`, `create` and `show`, and reading their output,
have still only met a stand-in.

**With a subscription**, the `subscription` job in `azure.yml` runs the runner for real:
a what-if, a deployment, its outputs, a second what-if that must show nothing, and an
update. It deploys one user-assigned managed identity, which costs nothing, and deletes
it. The job is skipped until the repository is given somewhere to deploy. It needs:

| Setting | Kind | Value |
|---|---|---|
| `AZURE_RESOURCE_GROUP` | repository **variable** | the name of an existing resource group the tests may deploy into. Setting this is what switches the job on |
| `AZURE_CLIENT_ID` | repository secret | the application (client) id of an app registration or user-assigned identity |
| `AZURE_TENANT_ID` | repository secret | the directory (tenant) id |
| `AZURE_SUBSCRIPTION_ID` | repository secret | the subscription the resource group is in |

The job signs in with OpenID Connect, so there is no password or client secret to store.
The app registration needs:

- a **federated credential** for this repository, with the subject
  `repo:ZanattaMichael/meridian-core:ref:refs/heads/main` for pushes to `main`, and one
  with `repo:ZanattaMichael/meridian-core:pull_request` for pull requests;
- the **Contributor** role on that one resource group, and nothing wider.

Pull requests from forks never run this job: they cannot sign in.

Until the job has run, the code in `plugins/bicep/tests/real.rs` and
`plugins/arm/tests/real.rs` that talks to Azure is itself untested, so expect its first
run to need attention.

### DSC

The `dsc` workflow has three parts.

**On Linux**, `plugins/dsc/tests/real.rs` runs the plugin's own runner against a real
PowerShell, the DscPipelineRunner module as published on the PowerShell Gallery, and
the `dsc` command for DSC v3. DSC v2 (`Invoke-DscResource`) does not work on Linux, where
PowerShell has no `libmi`, so it is tested on Windows only; set
`MERIDIAN_TEST_REAL_DSC_V3=1` or `MERIDIAN_TEST_REAL_DSC_V2=1` beside the switch to say
which engines the machine has. The
resource is `MeridianTestDsc/MarkerFile`, a small class-based DSC resource kept beside the
tests (`plugins/dsc/tests/modules`), whose state is a file in the test's scratch
directory. It records every `Set`, which is how the tests prove that a `refresh` notify
really sets the notified resource again.

**On Windows, the same tests** run with both engines asked for
(`MERIDIAN_TEST_REAL_DSC_V2=1` and `MERIDIAN_TEST_REAL_DSC_V3=1`), so the plugin's own
Rust runner drives DSC v2 and DSC v3 on a Windows machine. The job first checks that
`Get-DscResource` lists the test resource, because that is how DSC v2 finds it. Expect
this job to take around ten minutes: the DSC v3 test alone takes several on a Windows
runner.

**On Windows, real Windows resources** are used as well: `PSDscResources/Environment` and
`PSDscResources/Registry` through DSC v2, and `Microsoft.Windows/Registry` through DSC
v3. The documents are compiled on a Linux runner and handed to the Windows runner, where
`plugins/dsc/tests/windows/Test-CompiledProject.ps1` makes the same `Invoke-DscRunner`
calls the plugin's runner makes and checks the machine's environment and registry
afterwards, which also shows that a project compiled on one system runs on the other.

What that leaves unproven: the plugin's runner has not been run against the real Windows
resources (only the compiled projects have), so the first job on Windows uses the test
resource and the second uses `Invoke-DscRunner` directly.
Reaching another machine over WinRM or SSH is not tested for real.
