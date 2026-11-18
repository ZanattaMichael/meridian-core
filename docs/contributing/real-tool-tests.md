# Real-tool tests

Every plugin has two kinds of test for the half that runs its tool.

- **Run tests** (`tests/run.rs`) drive the runner against a stand-in: a script that
  prints what the tool is believed to print. They need nothing installed, run
  everywhere, and pin down the exact commands, environment and error handling.
- **Real-tool tests** (`tests/real.rs`, and a few other files) run the tool itself.
  Only they prove the stand-in is right.

This page lists the real-tool tests: what each one runs, what that proves, what it cannot
prove, and how to run it yourself.

## How they are switched on

Real-tool tests are marked `#[ignore]`, so `cargo test --workspace` needs only Rust
and reports them as ignored, not passed. Run them with `-- --ignored` and switch each
on with `MERIDIAN_TEST_REAL_<TOOL>`, read through
`meridian_plugin_testkit::real_tool_asked`. Unset, empty or `0` means off: the test
prints `skipped: set MERIDIAN_TEST_REAL_<TOOL>=1` and returns. A few switches take a
value instead of `1` (see the table). If the tool is not on `PATH`, `MERIDIAN_<TOOL>`
names it, as for a user.

Each has its own workflow under `.github/workflows/` that installs the tool, sets the
variable and runs the test file with `-- --ignored`. Tests using `real_tool_asked`
also honour `MERIDIAN_TEST_REAL_REQUIRED`: when set, a missing switch fails instead of
skipping, so a workflow that forgets its variable goes red. `puppet.yml` sets it; older
tests read their switch directly and should move to the helper when next touched.
Workflows are limited by `paths:` to the plugin and its dependencies. A test in
`crates/meridian-all-targets` fails if a plugin has an ignored test but no workflow.

To run one yourself:

```
MERIDIAN_TEST_REAL_ANSIBLE=1 cargo test -p meridian-target-ansible --test real -- --ignored --nocapture
```

Two rules keep a green job honest:

- **Asked for and missing is a failure.** With the variable set, a missing tool, an
  unreachable cluster or an unimportable module fails the test. A job cannot pass by
  checking nothing.
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
| `docker` | `docker.yml` | the Docker daemon and Compose | `MERIDIAN_TEST_REAL_DOCKER=1` |
| `dsc` | `dsc.yml` | PowerShell, DscPipelineRunner and DSC v3 on Linux; the plugin's runner with DSC v2 and DSC v3, and real Windows resources, [on Windows](#dsc) | `MERIDIAN_TEST_REAL_DSC=1` |
| `helm` | `helm.yml` | `helm`, `kustomize` and a kind cluster | none: `-- --ignored` alone runs them |
| `kubernetes` | `kubernetes.yml` | `kubectl` and a kind cluster | `MERIDIAN_TEST_REAL_KUBECTL=1` |
| `opentofu` | `opentofu.yml` | `tofu` | `MERIDIAN_TEST_REAL_TOFU=tofu` |
| `openvox` | `openvox.yml` | `puppet` from the OpenVox gem | `MERIDIAN_TEST_REAL_OPENVOX=1` |
| `packer` | `packer.yml` | `packer` | none: `-- --ignored` alone runs them |
| `podman` | `podman.yml` | Podman and systemd | `MERIDIAN_TEST_REAL_PODMAN=system` |
| `pulumi` | `pulumi.yml` | `pulumi` with a local backend | `MERIDIAN_TEST_REAL_PULUMI=1` |
| `puppet` | `puppet.yml` | `puppet` from the gem, applying on the machine itself; Bolt has only met a stand-in | `MERIDIAN_TEST_REAL_PUPPET=1` |
| `salt` | `salt.yml` | `salt-call` and `salt-ssh` | `MERIDIAN_TEST_REAL_SALT_CALL=<program>` |
| `terraform` | `terraform.yml` | `terraform`, and the same tests again with `tofu` | `MERIDIAN_TEST_REAL_TERRAFORM=1` |

Each target page's running section says what its real-tool test covers. Targets that
export [tool schemas](../tooling/schema-sync.md) have a schema export test in the same
file, under the same switch, that runs the export and checks a known type:

- against the installed tool for terraform, opentofu, kubernetes, helm, pulumi, dsc,
  ansible, puppet, openvox, chef and salt;
- against the vendor's published file, fetched over the network, for cloudformation,
  bicep, arm, cloud-init and docker (no tool or account needed).

The docker test also checks the attribute names the compiler uses against the Compose
schema, so a name the specification drops fails the build.

## The vertical slice

The tests above run one tool each, through its plugin's runner. One workflow is also
applied end to end, through the real `meridian` binary and the real plugin binaries,
with three tools at once: [`examples/workflow/vertical-slice.yaml`](../../examples/workflow/vertical-slice.yaml),
walked through in [worked workflows](../quickstart/workflows.md#vertical-slice).
`vertical-slice.yml` starts a kind cluster, installs Terraform and `ansible-core`, and
runs [`crates/meridian-cli/tests/vertical_slice.rs`](../../crates/meridian-cli/tests/vertical_slice.rs)
with `MERIDIAN_TEST_REAL_SLICE=1`.

What it proves that per-tool tests cannot: a value from one stage reaches the next
through the orchestrator and command line, with nothing stubbed. The Kubernetes
stage's `workload` output is imported by the Terraform stage as `TF_VAR_workload`; the
Terraform stage's `address` output becomes the Ansible stage's host. The test checks
what each tool left behind (the Deployment and ConfigMap, Terraform's file, Ansible's
file and marker), that the record and report name every stage, and that a second
`plan` and a `drift` find everything in sync.

What it does not prove: anything about a cloud. The cluster is kind in Docker,
Terraform uses `terraform_data` and `hashicorp/local`, and Ansible uses the local
connection, so no credentials are needed or read. A slice on real infrastructure (a
machine Terraform creates and Ansible reaches over SSH) needs an account and budget,
and is the owner's to set up; the [Azure job](#azure) shows its shape.

To run it yourself:

```
kind create cluster --wait 180s
MERIDIAN_TEST_REAL_SLICE=1 cargo test -p meridian-cli --test vertical_slice -- --ignored --nocapture
```

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

When the real tool disagrees with the stand-in, fix the runner, correct the stand-in
in `tests/run.rs` to match the real output, and describe the finding in the pull
request.

## Notes on particular targets

### CloudFormation

The tests run the real `aws` command line, pointed at
[LocalStack](https://github.com/localstack/localstack) by
`MERIDIAN_AWS_ARGS='--endpoint-url http://localhost:4566'`. LocalStack is an AWS
emulator running in a container on the runner. It creates the stack's queue, topic and
parameter, so no account or secret is needed.

What that proves: CloudFormation accepts the template, the plugin's change-set calls
work end to end, and previews, applies, no-ops, updates and deletions are each reported
correctly.

What it cannot prove: that the real service matches the emulator in every detail,
especially change-set timing and how replacements look. Two known differences:
LocalStack 4.4 reported an unchanged resource as an update (4.14, the pinned version,
does not); and LocalStack accepts nonexistent resource types that CloudFormation
refuses, so the tests' refusal case uses a reference to a missing resource, which both
refuse identically.

To run against a real account, sign in the `aws` command, leave `MERIDIAN_AWS_ARGS`
unset and set `MERIDIAN_TEST_REAL_CLOUDFORMATION=1`. The tests create and delete a
stack named `meridian-real` in `us-east-1`.

### Azure

Azure has no emulator and no way to preview a deployment offline: `what-if` is a call to
Resource Manager. So the Bicep and ARM plugins are tested in two parts.

**Without an account**, on every pull request that touches them:

- Bicep: the real Bicep compiler builds every file the plugin compiles. It parses the
  file, checks each resource against Azure's published types, and produces the ARM
  template that would be sent. The test also checks that template holds the requested
  resources, names and values.
- ARM: nothing validates an ARM template offline as Resource Manager does. The closest
  tool is the Bicep compiler's ARM reader (`az bicep decompile`), which must parse the
  whole template to convert it: schema, scope, every resource, expression and parameter
  reference. Every compiled template goes through it, and a deliberately broken one
  must be refused. One reader complaint is allowed: a template that names another
  resource by id (in `dependsOn`, or the `scope` of an extension resource such as a
  lock) cannot always be turned into a Bicep symbol. By then the reader has parsed the
  whole template. Whether Azure accepts those ids needs a subscription to prove.

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

**On Linux**, `plugins/dsc/tests/real.rs` runs the plugin's runner against real
PowerShell, the DscPipelineRunner module from the PowerShell Gallery, and the `dsc`
command for DSC v3. DSC v2 (`Invoke-DscResource`) needs `libmi`, which PowerShell on
Linux lacks, so it is tested on Windows only. Set `MERIDIAN_TEST_REAL_DSC_V3=1` or
`MERIDIAN_TEST_REAL_DSC_V2=1` alongside the switch to say which engines the machine
has. The resource is `MeridianTestDsc/MarkerFile`, a small class-based DSC resource in
`plugins/dsc/tests/modules` whose state is a file in the test's scratch directory. It
records every `Set`, proving that a `refresh` notify really re-sets the notified
resource.

**On Windows, the same tests** run with both engines asked for
(`MERIDIAN_TEST_REAL_DSC_V2=1` and `MERIDIAN_TEST_REAL_DSC_V3=1`), so the plugin's own
Rust runner drives DSC v2 and DSC v3 on a Windows machine. The job first checks that
`Get-DscResource` lists the test resource, because that is how DSC v2 finds it. Expect
this job to take around ten minutes: the DSC v3 test alone takes several on a Windows
runner.

**On Windows, real Windows resources** are also used: `PSDscResources/Environment` and
`PSDscResources/Registry` through DSC v2, and `Microsoft.Windows/Registry` through DSC
v3. Documents are compiled on a Linux runner and passed to the Windows runner, where
`plugins/dsc/tests/windows/Test-CompiledProject.ps1` makes the same `Invoke-DscRunner`
calls as the plugin's runner and checks the environment and registry afterwards. This
also shows a project compiled on one system runs on the other.

Still unproven: the plugin's runner itself has not run against real Windows resources
(only the compiled projects have), so the first Windows job uses the test resource and
the second calls `Invoke-DscRunner` directly.
Reaching another machine over WinRM or SSH is not tested for real.
