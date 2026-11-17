//! The capability matrix in the documentation is generated from what each
//! plugin declares, and this test fails when the two differ. Record it again
//! with `UPDATE_GOLDEN=1` after a plugin's capabilities change on purpose.

use std::fs;

use meridian_plugin_testkit::repo_root;
use meridian_sdk::{Emitter, Importer, Runner};

const PATH: &str = "docs/reference/capability-matrix.md";

const INTRODUCTION: &str = "\
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

";

fn row(emitter: &dyn Emitter, runner: &dyn Runner, importer: Option<&dyn Importer>) -> String {
    let capabilities = emitter.capabilities();
    let notifies = match (capabilities.native_notify, capabilities.synthetic_notify) {
        (true, true) => "native and synthetic",
        (true, false) => "native",
        (false, true) => "synthetic",
        (false, false) => "no",
    };
    let yes = |b: bool| if b { "yes" } else { "no" };
    let operations = runner.operations();
    let types: Vec<String> = emitter
        .supported_resource_types()
        .iter()
        .map(|t| format!("`{}`", t.replace('<', "‹").replace('>', "›")))
        .collect();
    let import = match importer {
        Some(importer) => importer
            .formats()
            .iter()
            .map(|f| f.description.clone())
            .collect::<Vec<_>>()
            .join("; "),
        None => "no".to_owned(),
    };
    format!(
        "| [{name}](../targets/{name}.md) | {category} | {types} | {notifies} | {runtime} | {plan} | {apply} | {outputs} | {inputs} | {import} |\n",
        name = emitter.name(),
        category = emitter.category(),
        types = types.join(", "),
        runtime = yes(capabilities.runtime_condition),
        plan = yes(operations.plan),
        apply = yes(operations.apply),
        outputs = yes(operations.outputs),
        inputs = yes(runner.accepts_inputs()),
    )
}

fn matrix() -> String {
    let mut rows = vec![
        row(
            &meridian_target_ansible::Ansible,
            &meridian_target_ansible::AnsibleRunner::from_env(),
            Some(&meridian_target_ansible::AnsibleImporter),
        ),
        row(
            &meridian_target_arm::Arm,
            &meridian_target_arm::ArmRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_bicep::Bicep,
            &meridian_target_bicep::BicepRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_chef::Chef,
            &meridian_target_chef::ChefRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_cloud_init::CloudInit,
            &meridian_target_cloud_init::CloudInitRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_cloudformation::CloudFormation,
            &meridian_target_cloudformation::CloudFormationRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_docker::Docker,
            &meridian_target_docker::DockerRunner::from_env(),
            Some(&meridian_target_docker::DockerImporter),
        ),
        row(
            &meridian_target_dsc::Dsc,
            &meridian_target_dsc::DscRunner::from_env(),
            Some(&meridian_target_dsc::DscImporter),
        ),
        row(
            &meridian_target_helm::Helm,
            &meridian_target_helm::HelmRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_kubernetes::Kubernetes,
            &meridian_target_kubernetes::KubernetesRunner::from_env(),
            Some(&meridian_target_kubernetes::KubernetesImporter),
        ),
        row(
            &meridian_target_opentofu::OpenTofu,
            &meridian_target_opentofu::OpenTofuRunner::from_env(),
            Some(&meridian_target_opentofu::OpenTofuImporter),
        ),
        row(
            &meridian_target_packer::Packer,
            &meridian_target_packer::PackerRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_podman::Podman,
            &meridian_target_podman::PodmanRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_pulumi::Pulumi,
            &meridian_target_pulumi::PulumiRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_openvox::OpenVox,
            &meridian_target_openvox::OpenVoxRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_puppet::Puppet,
            &meridian_target_puppet::PuppetRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_salt::Salt,
            &meridian_target_salt::SaltRunner::from_env(),
            None,
        ),
        row(
            &meridian_target_terraform::Terraform,
            &meridian_target_terraform::TerraformRunner::from_env(),
            Some(&meridian_target_terraform::TerraformImporter),
        ),
    ];
    rows.sort();
    format!(
        "{INTRODUCTION}| Target | Category | Resource types | notifies | runtimeWhen | plan | apply | outputs | inputs | import |\n|---|---|---|---|---|---|---|---|---|---|\n{}",
        rows.concat()
    )
}

#[test]
fn the_documented_matrix_is_what_the_plugins_declare() {
    let path = repo_root().join(PATH);
    let generated = matrix();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &generated).unwrap();
    }
    let documented = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        documented == generated,
        "{PATH} is out of date with what the plugins declare; run the tests with UPDATE_GOLDEN=1 to record it again"
    );
}
