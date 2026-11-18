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
- **import** is what `meridian import` can read of the tool's existing
  configuration and turn into a document for the target. See
  [Importing existing configuration](../operations/importing.md).
- **schemas** says the plugin exports its tool's own resource schemas, so
  `meridian schema sync` can cache them for the editor tools. See
  [Tool schemas](../tooling/schema-sync.md).

| Target | Category | Resource types | notifies | runtimeWhen | plan | apply | outputs | inputs | secrets | credentials | import | schemas |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| [ansible](../targets/ansible.md) | configuration-management | `‹namespace›.‹collection›.‹module›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | yes | no | no | yes | yes | Ansible playbooks and task files | yes |
| [arm](../targets/arm.md) | infrastructure-as-code | `‹Namespace›/‹type›`, `deployment`, `parameter` | no | no | yes | yes | yes | yes | yes | no | no | yes |
| [bicep](../targets/bicep.md) | infrastructure-as-code | `‹Namespace›/‹type›@‹api-version›`, `address`, `deployment`, `firewall`, `network`, `subnet`, `vm` | no | no | yes | yes | yes | no | yes | no | no | yes |
| [chef](../targets/chef.md) | configuration-management | `‹resource›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | yes | no | no | yes | yes | no | yes |
| [cloud-init](../targets/cloud-init.md) | configuration-management | `‹module›`, `exec`, `file`, `group`, `package`, `service`, `user` | synthetic | no | yes | yes | yes | no | no | no | no | yes |
| [cloudformation](../targets/cloudformation.md) | infrastructure-as-code | `‹Vendor›::‹Service›::‹Type›`, `stack` | no | no | yes | yes | yes | no | yes | no | no | yes |
| [docker](../targets/docker.md) | containerization | `config`, `network`, `project`, `secret`, `service`, `volume` | native and synthetic | no | yes | yes | yes | no | yes | no | Compose files | yes |
| [dsc](../targets/dsc.md) | configuration-management | `‹Module›/‹Resource›`, `runner` | native | yes | yes | yes | no | no | no | no | PowerShell DSC configurations; DSC v3 configuration documents and DscPipelineRunner projects | yes |
| [helm](../targets/helm.md) | container-orchestration | `‹apiVersion›/‹Kind›`, `chart`, `kustomization`, `release`, `values` | synthetic | no | yes | yes | yes | no | yes | no | no | yes |
| [kubernetes](../targets/kubernetes.md) | container-orchestration | `‹apiVersion›/‹Kind›`, `cluster` | synthetic | no | yes | yes | yes | no | yes | no | Kubernetes manifests | yes |
| [opentofu](../targets/opentofu.md) | infrastructure-as-code | `‹provider›_‹type›`, `address`, `backend`, `data.‹type›`, `encryption`, `ephemeral.‹type›`, `firewall`, `local`, `module`, `network`, `provider`, `subnet`, `variable`, `vm` | synthetic | no | yes | yes | yes | yes | yes | no | OpenTofu configuration files; OpenTofu configuration files in the JSON syntax | yes |
| [openvox](../targets/openvox.md) | configuration-management | `‹module›::‹type›`, `‹type›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | no | yes | yes | no | no | yes | yes | no | yes |
| [packer](../targets/packer.md) | infrastructure-as-code | `data.‹type›`, `error-cleanup-provisioner.‹type›`, `local`, `plugin`, `post-processor.‹type›`, `provisioner.‹type›`, `source.‹builder›`, `variable` | no | no | yes | yes | yes | yes | yes | no | no | no |
| [podman](../targets/podman.md) | containerization | `build`, `container`, `image`, `kube`, `network`, `pod`, `volume` | native | no | yes | yes | yes | no | yes | yes | no | no |
| [pulumi](../targets/pulumi.md) | infrastructure-as-code | `‹package›:‹module›:‹Type›`, `config`, `invoke.‹function›`, `stack` | no | no | yes | yes | yes | yes | yes | no | no | yes |
| [puppet](../targets/puppet.md) | configuration-management | `‹module›::‹type›`, `‹type›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | no | yes | yes | no | no | yes | yes | no | yes |
| [salt](../targets/salt.md) | configuration-management | `‹module›.‹function›`, `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | yes | no | no | yes | yes | no | yes |
| [terraform](../targets/terraform.md) | infrastructure-as-code | `‹provider›_‹type›`, `address`, `backend`, `data.‹type›`, `firewall`, `local`, `module`, `network`, `provider`, `subnet`, `variable`, `vm` | synthetic | no | yes | yes | yes | yes | yes | no | Terraform configuration files; Terraform configuration files in the JSON syntax | yes |
