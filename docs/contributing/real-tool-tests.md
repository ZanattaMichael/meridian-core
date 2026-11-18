# Real-tool tests

- **Run tests** (`tests/run.rs`) drive the runner against a stand-in script. They need
  nothing installed.
- **Real-tool tests** (`tests/real.rs` and a few others) run the tool itself, proving the
  stand-in is right.

## How they are switched on

They are `#[ignore]`d. Run with `-- --ignored` and `MERIDIAN_TEST_REAL_<TOOL>=1` (read by
`meridian_plugin_testkit::real_tool_asked`; unset or `0` skips). `MERIDIAN_<TOOL>` names the
binary if it is not on `PATH`.

Each has a workflow under `.github/workflows/`, limited by `paths:`. With
`MERIDIAN_TEST_REAL_REQUIRED` set, a missing switch fails instead of skipping. A test in
`crates/meridian-all-targets` fails if a plugin has ignored tests but no workflow.

Locally:

```
MERIDIAN_TEST_REAL_ANSIBLE=1 cargo test -p meridian-target-ansible --test real -- --ignored --nocapture
```

- **Asked for and missing is a failure.**
- **Each workflow prints and, where possible, pins the tool version.**

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

[Schema export](../tooling/schema-sync.md) tests use the same switch: against the installed
tool for terraform, opentofu, kubernetes, helm, pulumi, dsc, ansible, puppet, openvox, chef
and salt; against the vendor's published file for cloudformation, bicep, arm, cloud-init and
docker. Docker also checks compiler attribute names against the Compose schema.

## The vertical slice

[`examples/workflow/vertical-slice.yaml`](../../examples/workflow/vertical-slice.yaml)
([walkthrough](../quickstart/workflows.md#vertical-slice)) runs Kubernetes (kind), Terraform
and Ansible end to end through the real binaries. `vertical-slice.yml` runs
[`vertical_slice.rs`](../../crates/meridian-cli/tests/vertical_slice.rs) with
`MERIDIAN_TEST_REAL_SLICE=1`.

It proves outputs flow between stages (`workload` to `TF_VAR_workload`, `address` to the
Ansible host), checks each tool's result and the record, and that a second `plan` and
`drift` are in sync. It uses no cloud or credentials.

Locally:

```
kind create cluster --wait 180s
MERIDIAN_TEST_REAL_SLICE=1 cargo test -p meridian-cli --test vertical_slice -- --ignored --nocapture
```

## What a real-tool test should do

1. Compile a small document.
2. **Plan**; check the report and that nothing was created.
3. **Apply**; check the real result (file, cluster object, stack).
4. **Plan again**; expect no changes.
5. Change the document; check the plan's action (update, replace, delete).
6. Give something the tool refuses; check the tool's reason is carried.
7. Clean up in a `Drop`.

Use free resources (`terraform_data`, a ConfigMap, a scratch file). When the tool disagrees
with the stand-in, fix the runner and the stand-in, and note it in the pull request.

## Notes on particular targets

### CloudFormation

The real `aws` CLI against [LocalStack](https://github.com/localstack/localstack)
(`MERIDIAN_AWS_ARGS='--endpoint-url http://localhost:4566'`), no account needed. Proves the
template and change-set flow. LocalStack accepts unknown resource types, so the refusal case
uses a missing reference.

For a real account: sign in `aws`, unset `MERIDIAN_AWS_ARGS`, set
`MERIDIAN_TEST_REAL_CLOUDFORMATION=1`. It creates and deletes stack `meridian-real` in
`us-east-1`.

### Azure

Azure has no emulator; `what-if` needs Resource Manager.

**Without an account** (every pull request):

- Bicep: the real compiler builds every file and the resulting template is checked.
- ARM: every template must parse with `az bicep decompile`, and a broken one must be
  refused. Complaints about resource ids that cannot become Bicep symbols are allowed.

This proves the output, **not the runner**.

**With a subscription**, the `subscription` job in `azure.yml` runs what-if, deploy, outputs,
a clean second what-if and an update, using a free managed identity it deletes. Skipped
until configured:

| Setting | Kind | Value |
|---|---|---|
| `AZURE_RESOURCE_GROUP` | repository **variable** | the name of an existing resource group the tests may deploy into. Setting this is what switches the job on |
| `AZURE_CLIENT_ID` | repository secret | the application (client) id of an app registration or user-assigned identity |
| `AZURE_TENANT_ID` | repository secret | the directory (tenant) id |
| `AZURE_SUBSCRIPTION_ID` | repository secret | the subscription the resource group is in |

It uses OpenID Connect. The app registration needs:

- a **federated credential** for this repository, with the subject
  `repo:ZanattaMichael/meridian-core:ref:refs/heads/main` for pushes to `main`, and one
  with `repo:ZanattaMichael/meridian-core:pull_request` for pull requests;
- the **Contributor** role on that one resource group, and nothing wider.

Fork pull requests cannot run it. Its Azure code is untested until the first run.

### DSC

- **Linux**: `plugins/dsc/tests/real.rs` runs against PowerShell, DscPipelineRunner and
  `dsc` (v3). v2 needs `libmi`, so Windows only. `MERIDIAN_TEST_REAL_DSC_V3=1` or `_V2=1`
  says which engines exist. The resource is `MeridianTestDsc/MarkerFile`
  (`plugins/dsc/tests/modules`), which records every `Set`.
- **Windows, same tests** with both engines (about ten minutes).
- **Windows, real resources** (`PSDscResources/Environment`, `PSDscResources/Registry`,
  `Microsoft.Windows/Registry`): compiled on Linux, applied on Windows by
  `plugins/dsc/tests/windows/Test-CompiledProject.ps1`.

Not yet proven: the plugin's runner on real Windows resources, and remote WinRM or SSH.
