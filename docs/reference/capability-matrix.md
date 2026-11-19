# Capability matrix

What each target can express, as each plugin itself declares it. This page is
generated: `crates/meridian-cli/tests/capability_matrix.rs` writes it from the
plugins and fails when it is out of date, so it cannot drift from the code.

A cell that says **no** is not a missing feature waiting to be papered over.
A document that asks a target for something it cannot do is refused when it is
compiled, with the reason. Each target's own page says how it does what it can.

- **notifies** is *native* when the tool has its own way to react to a change,
  and *synthetic* when Meridian builds the reaction out of something else the
  tool has. A synthetic notify shows up in the tool's own output, and the
  target's page says what it looks like.
- **runtimeWhen** is a condition the tool checks on the node at the moment it
  applies a resource.
- **plan** asks the tool what an apply would change, without changing it.
- **saved plan** says the tool saves its plan when `meridian plan --out` is
  run, and applies exactly that plan on `meridian apply --plan`. Every target
  is checked by its artifact hash; this column says which also hand the tool
  its own plan back. See [a saved plan](../operations/pull-requests.md).
- **outputs** are values a later stage of a workflow can read.
- **inputs** says the target hands its tool the values a document's
  `spec.imports` brings from other documents, as the tool's own variables or
  parameters.
- **secrets** says a param may hold a secret: the target compiles a
  reference to it, never the value, and hands the value to its tool by a
  private channel when it runs. See
  [Secrets](dsl/secrets.md#handing-a-secret-to-a-tool).
- **credentials** says the target reaches its host as the credential
  `spec.targetHost.credential` names: it compiles the credential's plain
  fields, such as the user, and hands its secret fields to the tool by a
  private channel when it runs. See
  [Credentials](dsl/credentials.md).
- **cloud** says the target reaches its cloud as the credential
  `spec.endpoint.credential` names (an `aws`, `azure` or `gcp` credential):
  it hands the credential to its tool in the tool's own environment, the
  one way the providers take a login, and the plan line says so. See
  [Reaching a cloud as a credential](dsl/credentials.md#reaching-a-cloud-as-a-credential).
- **adopt** is what the target does with a resource's `adopt`: *import*
  compiles it into the tool's own import, so the tool takes the existing
  thing into its state; *converge* means the tool keeps no state and makes
  the thing what the document says whether it existed or not, so `adopt`
  changes nothing; *no* refuses it. See
  [Adopting what exists](dsl/resources.md#adopting-what-exists).
- **import** is what `meridian import` can read of the tool's existing
  configuration and turn into a document for the target. See
  [Importing existing configuration](../operations/importing.md).
- **schemas** says the plugin exports its tool's own resource schemas, so
  `meridian schema sync` can cache them for the editor tools. See
  [Tool schemas](../tooling/schema-sync.md).

| Target | Category | Resource types | notifies | runtimeWhen | plan | saved plan | apply | outputs | inputs | secrets | credentials | cloud | adopt | import | schemas |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| [ansible](../targets/ansible.md) | configuration-management | `‹namespace›.‹collection›.‹module›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | no | yes | no | no | yes | yes | no | converge | Ansible playbooks and task files | yes |
| [arm](../targets/arm.md) | infrastructure-as-code | `‹Namespace›/‹type›`, `address`, `deployment`, `firewall`, `network`, `parameter`, `subnet`, `vm` | no | no | yes | no | yes | yes | yes | yes | no | no | converge | no | yes |
| [bicep](../targets/bicep.md) | infrastructure-as-code | `‹Namespace›/‹type›@‹api-version›`, `address`, `deployment`, `firewall`, `network`, `subnet`, `vm` | no | no | yes | no | yes | yes | no | yes | no | no | converge | no | yes |
| [chef](../targets/chef.md) | configuration-management | `‹resource›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | no | yes | no | no | yes | yes | no | converge | no | yes |
| [cloud-init](../targets/cloud-init.md) | configuration-management | `‹module›`, `exec`, `file`, `group`, `package`, `service`, `user` | synthetic | no | yes | no | yes | yes | no | no | no | no | no | no | yes |
| [cloudformation](../targets/cloudformation.md) | infrastructure-as-code | `‹Vendor›::‹Service›::‹Type›`, `address`, `firewall`, `network`, `stack`, `subnet`, `vm` | no | no | yes | yes | yes | yes | no | yes | no | yes | no | no | yes |
| [docker](../targets/docker.md) | containerization | `config`, `network`, `project`, `secret`, `service`, `volume` | native and synthetic | no | yes | no | yes | yes | no | yes | no | no | no | Compose files | yes |
| [dsc](../targets/dsc.md) | configuration-management | `‹Module›/‹Resource›`, `runner` | native | yes | yes | no | yes | no | no | no | no | no | converge | PowerShell DSC configurations; DSC v3 configuration documents and DscPipelineRunner projects | yes |
| [helm](../targets/helm.md) | container-orchestration | `‹apiVersion›/‹Kind›`, `chart`, `kustomization`, `release`, `values` | synthetic | no | yes | no | yes | yes | no | yes | no | no | no | no | yes |
| [kubernetes](../targets/kubernetes.md) | container-orchestration | `‹apiVersion›/‹Kind›`, `cluster` | synthetic | no | yes | no | yes | yes | no | yes | no | no | converge | Kubernetes manifests | yes |
| [opentofu](../targets/opentofu.md) | infrastructure-as-code | `‹provider›_‹type›`, `address`, `backend`, `data.‹type›`, `encryption`, `ephemeral.‹type›`, `firewall`, `local`, `module`, `network`, `provider`, `subnet`, `variable`, `vm` | synthetic | no | yes | yes | yes | yes | yes | yes | no | yes | import | OpenTofu configuration files; OpenTofu configuration files in the JSON syntax | yes |
| [openvox](../targets/openvox.md) | configuration-management | `‹module›::‹type›`, `‹type›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | no | yes | no | yes | no | no | yes | yes | no | converge | no | yes |
| [packer](../targets/packer.md) | infrastructure-as-code | `data.‹type›`, `error-cleanup-provisioner.‹type›`, `local`, `plugin`, `post-processor.‹type›`, `provisioner.‹type›`, `source.‹builder›`, `variable` | no | no | yes | no | yes | yes | yes | yes | no | yes | no | no | no |
| [podman](../targets/podman.md) | containerization | `build`, `container`, `image`, `kube`, `network`, `pod`, `volume` | native | no | yes | no | yes | yes | no | yes | yes | no | no | no | no |
| [pulumi](../targets/pulumi.md) | infrastructure-as-code | `‹package›:‹module›:‹Type›`, `address`, `config`, `firewall`, `invoke.‹function›`, `network`, `stack`, `subnet`, `vm` | no | no | yes | yes | yes | yes | yes | yes | no | yes | import | no | yes |
| [puppet](../targets/puppet.md) | configuration-management | `‹module›::‹type›`, `‹type›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | no | yes | no | yes | no | no | yes | yes | no | converge | no | yes |
| [salt](../targets/salt.md) | configuration-management | `‹module›.‹function›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | no | yes | no | no | yes | yes | no | converge | no | yes |
| [terraform](../targets/terraform.md) | infrastructure-as-code | `‹provider›_‹type›`, `address`, `backend`, `data.‹type›`, `firewall`, `local`, `module`, `network`, `provider`, `subnet`, `variable`, `vm` | synthetic | no | yes | yes | yes | yes | yes | yes | no | yes | import | Terraform configuration files; Terraform configuration files in the JSON syntax | yes |

## `--tool-debug`

What each level of [`--tool-debug`](cli.md#asking-a-tool-for-more-output)
adds to the target's tool. A level above the last one a target has uses its
last; *none* means the target has no debug output and the flag is warned
about.

| Target | 1 | 2 | 3 |
|---|---|---|---|
| [ansible](../targets/ansible.md) | `-v` | `-vvv` | `-vvvv` |
| [arm](../targets/arm.md) | `--verbose` | `--debug` | as 2 |
| [bicep](../targets/bicep.md) | `--verbose` | `--debug` | as 2 |
| [chef](../targets/chef.md) | `-l info; chef-run: [log] level info` | `-l debug; chef-run: [log] level debug` | `-l trace; chef-run: [log] level debug` |
| [cloud-init](../targets/cloud-init.md) | `--debug` | as 1 | as 1 |
| [cloudformation](../targets/cloudformation.md) | `--debug` | as 1 | as 1 |
| [docker](../targets/docker.md) | `--log-level debug` | as 1 | as 1 |
| [dsc](../targets/dsc.md) | `$VerbosePreference = 'Continue'` | `$VerbosePreference = 'Continue' $DebugPreference = 'Continue'` | as 2 |
| [helm](../targets/helm.md) | `--debug` | as 1 | as 1 |
| [kubernetes](../targets/kubernetes.md) | `-v=4` | `-v=6` | `-v=8` |
| [opentofu](../targets/opentofu.md) | `TF_LOG=INFO` | `TF_LOG=DEBUG` | `TF_LOG=TRACE` |
| [openvox](../targets/openvox.md) | `--evaltrace; bolt: --log-level info` | `--debug; bolt: --log-level debug` | `--debug --trace; bolt: --log-level trace` |
| [packer](../targets/packer.md) | `PACKER_LOG=1` | as 1 | as 1 |
| [podman](../targets/podman.md) | `100 journal lines of a unit that fails` | `100 journal lines and systemctl status of a unit that fails` | `100 journal lines and systemctl status of a unit that fails, and every unit Quadlet generates` |
| [pulumi](../targets/pulumi.md) | `-v=3 --logtostderr` | `-v=6 --logtostderr` | `-v=9 --logtostderr` |
| [puppet](../targets/puppet.md) | `--evaltrace; bolt: --log-level info` | `--debug; bolt: --log-level debug` | `--debug --trace; bolt: --log-level trace` |
| [salt](../targets/salt.md) | `-l info` | `-l debug` | `-l trace` |
| [terraform](../targets/terraform.md) | `TF_LOG=INFO` | `TF_LOG=DEBUG` | `TF_LOG=TRACE` |
