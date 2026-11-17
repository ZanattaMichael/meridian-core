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
- **import** is what `meridian import` can read of the tool's existing
  configuration and turn into a document for the target. See
  [Importing existing configuration](../operations/importing.md).

| Target | Category | Resource types | notifies | runtimeWhen | plan | apply | outputs | inputs | import |
|---|---|---|---|---|---|---|---|---|---|
| [ansible](../targets/ansible.md) | configuration-management | `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | yes | no | no | Ansible playbooks and task files |
| [arm](../targets/arm.md) | infrastructure-as-code | `‹Namespace›/‹type›`, `deployment`, `parameter` | no | no | yes | yes | yes | yes | no |
| [bicep](../targets/bicep.md) | infrastructure-as-code | `‹Namespace›/‹type›@‹api-version›`, `deployment` | no | no | yes | yes | yes | no | no |
| [chef](../targets/chef.md) | configuration-management | `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | yes | no | no | no |
| [cloud-init](../targets/cloud-init.md) | configuration-management | `exec`, `file`, `group`, `package`, `service`, `user` | synthetic | no | yes | yes | yes | no | no |
| [cloudformation](../targets/cloudformation.md) | infrastructure-as-code | `‹Vendor›::‹Service›::‹Type›`, `stack` | no | no | yes | yes | yes | no | no |
| [docker](../targets/docker.md) | containerization | `config`, `network`, `project`, `secret`, `service`, `volume` | native and synthetic | no | yes | yes | yes | no | Compose files |
| [dsc](../targets/dsc.md) | configuration-management | `‹Module›/‹Resource›`, `runner` | native | yes | yes | yes | no | no | PowerShell DSC configurations; DSC v3 configuration documents and DscPipelineRunner projects |
| [helm](../targets/helm.md) | container-orchestration | `‹apiVersion›/‹Kind›`, `chart`, `kustomization`, `release`, `values` | synthetic | no | yes | yes | yes | no | no |
| [kubernetes](../targets/kubernetes.md) | container-orchestration | `‹apiVersion›/‹Kind›`, `cluster` | synthetic | no | yes | yes | yes | no | Kubernetes manifests |
| [opentofu](../targets/opentofu.md) | infrastructure-as-code | `‹provider›_‹type›`, `backend`, `data.‹type›`, `encryption`, `ephemeral.‹type›`, `local`, `module`, `provider`, `variable` | synthetic | no | yes | yes | yes | yes | OpenTofu configuration files; OpenTofu configuration files in the JSON syntax |
| [openvox](../targets/openvox.md) | configuration-management | `exec`, `file`, `group`, `package`, `service`, `user` | native | no | yes | yes | no | no | no |
| [packer](../targets/packer.md) | infrastructure-as-code | `data.‹type›`, `error-cleanup-provisioner.‹type›`, `local`, `plugin`, `post-processor.‹type›`, `provisioner.‹type›`, `source.‹builder›`, `variable` | no | no | yes | yes | yes | yes | no |
| [podman](../targets/podman.md) | containerization | `build`, `container`, `image`, `kube`, `network`, `pod`, `volume` | native | no | yes | yes | yes | no | no |
| [pulumi](../targets/pulumi.md) | infrastructure-as-code | `‹package›:‹module›:‹Type›`, `config`, `invoke.‹function›`, `stack` | no | no | yes | yes | yes | yes | no |
| [puppet](../targets/puppet.md) | configuration-management | `exec`, `file`, `group`, `package`, `service`, `user` | native | no | yes | yes | no | no | no |
| [salt](../targets/salt.md) | configuration-management | `exec`, `file`, `group`, `package`, `service`, `user` | native | yes | yes | yes | no | no | no |
| [terraform](../targets/terraform.md) | infrastructure-as-code | `‹provider›_‹type›`, `backend`, `data.‹type›`, `local`, `module`, `provider`, `variable` | synthetic | no | yes | yes | yes | yes | Terraform configuration files; Terraform configuration files in the JSON syntax |
