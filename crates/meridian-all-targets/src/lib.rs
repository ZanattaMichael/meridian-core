//! Every target plugin the workspace ships, as one list.
//!
//! Tests that run "every target" (conformance, the capability matrix, the
//! editor catalogue) and documentation that is generated from the plugins
//! read this list instead of each keeping its own. Adding a plugin means
//! adding one [`Target`] here; `tests/complete.rs` fails when a directory
//! under `plugins/` has no entry, and when a target is missing from the
//! pages that list them all.
//!
//! The crate is for the workspace's own tests and tools. The `meridian`
//! binary does not depend on it: it finds plugins on disk.

use meridian_sdk::{Emitter, Importer, Runner};

/// One shipped plugin: how to make each of its parts in-process.
pub struct Target {
    /// The name documents select with `spec.target`, which is also the
    /// directory under `plugins/`.
    pub name: &'static str,
    pub emitter: fn() -> Box<dyn Emitter + Send + Sync>,
    /// The runner as its environment variables configure it.
    pub runner: fn() -> Box<dyn Runner>,
    /// The importer, for a plugin that has one.
    pub importer: Option<fn() -> Box<dyn Importer + Send + Sync>>,
}

impl Target {
    pub fn emitter(&self) -> Box<dyn Emitter + Send + Sync> {
        (self.emitter)()
    }

    pub fn runner(&self) -> Box<dyn Runner> {
        (self.runner)()
    }

    pub fn importer(&self) -> Option<Box<dyn Importer + Send + Sync>> {
        self.importer.map(|make| make())
    }
}

macro_rules! target {
    ($name:literal, $krate:ident, $emitter:ident, $runner:ident) => {
        Target {
            name: $name,
            emitter: || Box::new($krate::$emitter),
            runner: || Box::new($krate::$runner::from_env()),
            importer: None,
        }
    };
    ($name:literal, $krate:ident, $emitter:ident, $runner:ident, $importer:ident) => {
        Target {
            name: $name,
            emitter: || Box::new($krate::$emitter),
            runner: || Box::new($krate::$runner::from_env()),
            importer: Some(|| Box::new($krate::$importer)),
        }
    };
}

/// Every shipped target, sorted by name.
pub const ALL: &[Target] = &[
    target!(
        "ansible",
        meridian_target_ansible,
        Ansible,
        AnsibleRunner,
        AnsibleImporter
    ),
    target!("arm", meridian_target_arm, Arm, ArmRunner),
    target!("bicep", meridian_target_bicep, Bicep, BicepRunner),
    target!("chef", meridian_target_chef, Chef, ChefRunner),
    target!(
        "cloud-init",
        meridian_target_cloud_init,
        CloudInit,
        CloudInitRunner
    ),
    target!(
        "cloudformation",
        meridian_target_cloudformation,
        CloudFormation,
        CloudFormationRunner
    ),
    target!(
        "docker",
        meridian_target_docker,
        Docker,
        DockerRunner,
        DockerImporter
    ),
    target!("dsc", meridian_target_dsc, Dsc, DscRunner, DscImporter),
    target!("helm", meridian_target_helm, Helm, HelmRunner),
    target!(
        "kubernetes",
        meridian_target_kubernetes,
        Kubernetes,
        KubernetesRunner,
        KubernetesImporter
    ),
    target!(
        "opentofu",
        meridian_target_opentofu,
        OpenTofu,
        OpenTofuRunner,
        OpenTofuImporter
    ),
    target!("openvox", meridian_target_openvox, OpenVox, OpenVoxRunner),
    target!("packer", meridian_target_packer, Packer, PackerRunner),
    target!("podman", meridian_target_podman, Podman, PodmanRunner),
    target!("pulumi", meridian_target_pulumi, Pulumi, PulumiRunner),
    target!("puppet", meridian_target_puppet, Puppet, PuppetRunner),
    target!("salt", meridian_target_salt, Salt, SaltRunner),
    target!(
        "terraform",
        meridian_target_terraform,
        Terraform,
        TerraformRunner,
        TerraformImporter
    ),
];

/// Every target's emitter, sorted by name.
pub fn emitters() -> Vec<Box<dyn Emitter + Send + Sync>> {
    ALL.iter().map(Target::emitter).collect()
}

/// The names of every target, sorted.
pub fn names() -> Vec<&'static str> {
    ALL.iter().map(|t| t.name).collect()
}

/// One target, by name.
pub fn target(name: &str) -> Option<&'static Target> {
    ALL.iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_sorted_and_each_name_is_the_plugins_own() {
        let names = names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "ALL must be sorted by name");
        for target in ALL {
            assert_eq!(target.emitter().name(), target.name);
        }
    }
}
