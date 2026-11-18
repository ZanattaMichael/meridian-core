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

use meridian_sdk::{Emitter, Importer, Runner, SchemaSource};

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
    /// The schema source, for a plugin that describes its tool's types.
    pub schemas: Option<fn() -> Box<dyn SchemaSource>>,
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

    pub fn schemas(&self) -> Option<Box<dyn SchemaSource>> {
        self.schemas.map(|make| make())
    }
}

macro_rules! target {
    ($name:literal, $krate:ident, $emitter:ident, $runner:ident) => {
        Target {
            name: $name,
            emitter: || Box::new($krate::$emitter),
            runner: || Box::new($krate::$runner::from_env()),
            importer: None,
            schemas: None,
        }
    };
    ($name:literal, $krate:ident, $emitter:ident, $runner:ident, $importer:ident) => {
        Target {
            name: $name,
            emitter: || Box::new($krate::$emitter),
            runner: || Box::new($krate::$runner::from_env()),
            importer: Some(|| Box::new($krate::$importer)),
            schemas: None,
        }
    };
}

/// Every shipped target, sorted by name.
pub const ALL: &[Target] = &[
    Target {
        name: "ansible",
        emitter: || Box::new(meridian_target_ansible::Ansible),
        runner: || Box::new(meridian_target_ansible::AnsibleRunner::from_env()),
        importer: Some(|| Box::new(meridian_target_ansible::AnsibleImporter)),
        schemas: Some(|| Box::new(meridian_target_ansible::AnsibleSchemas::from_env())),
    },
    Target {
        name: "arm",
        emitter: || Box::new(meridian_target_arm::Arm),
        runner: || Box::new(meridian_target_arm::ArmRunner::from_env()),
        importer: None,
        // ARM reads the same Azure type definitions as Bicep, with the API
        // version as a param.
        schemas: Some(|| {
            Box::new(meridian_target_bicep::AzureSchemas::new(
                meridian_target_bicep::Dialect::Arm,
            ))
        }),
    },
    Target {
        name: "bicep",
        emitter: || Box::new(meridian_target_bicep::Bicep),
        runner: || Box::new(meridian_target_bicep::BicepRunner::from_env()),
        importer: None,
        schemas: Some(|| {
            Box::new(meridian_target_bicep::AzureSchemas::new(
                meridian_target_bicep::Dialect::Bicep,
            ))
        }),
    },
    target!("chef", meridian_target_chef, Chef, ChefRunner),
    Target {
        name: "cloud-init",
        emitter: || Box::new(meridian_target_cloud_init::CloudInit),
        runner: || Box::new(meridian_target_cloud_init::CloudInitRunner::from_env()),
        importer: None,
        schemas: Some(|| Box::new(meridian_target_cloud_init::CloudInitSchemas::from_env())),
    },
    Target {
        name: "cloudformation",
        emitter: || Box::new(meridian_target_cloudformation::CloudFormation),
        runner: || Box::new(meridian_target_cloudformation::CloudFormationRunner::from_env()),
        importer: None,
        schemas: Some(|| {
            Box::new(meridian_target_cloudformation::CloudFormationSchemas::from_env())
        }),
    },
    Target {
        name: "docker",
        emitter: || Box::new(meridian_target_docker::Docker),
        runner: || Box::new(meridian_target_docker::DockerRunner::from_env()),
        importer: Some(|| Box::new(meridian_target_docker::DockerImporter)),
        schemas: Some(|| Box::new(meridian_target_docker::ComposeSchemas::from_env())),
    },
    Target {
        name: "dsc",
        emitter: || Box::new(meridian_target_dsc::Dsc),
        runner: || Box::new(meridian_target_dsc::DscRunner::from_env()),
        importer: Some(|| Box::new(meridian_target_dsc::DscImporter)),
        schemas: Some(|| Box::new(meridian_target_dsc::DscSchemas::from_env())),
    },
    Target {
        name: "helm",
        emitter: || Box::new(meridian_target_helm::Helm),
        runner: || Box::new(meridian_target_helm::HelmRunner::from_env()),
        importer: None,
        // A chart's objects are described by the cluster, as for
        // kubernetes, through this target's kubectl settings.
        schemas: Some(|| Box::new(meridian_target_helm::HelmSchemas::from_env())),
    },
    Target {
        name: "kubernetes",
        emitter: || Box::new(meridian_target_kubernetes::Kubernetes),
        runner: || Box::new(meridian_target_kubernetes::KubernetesRunner::from_env()),
        importer: Some(|| Box::new(meridian_target_kubernetes::KubernetesImporter)),
        schemas: Some(|| Box::new(meridian_target_kubernetes::KubernetesSchemas::from_env())),
    },
    Target {
        name: "opentofu",
        emitter: || Box::new(meridian_target_opentofu::OpenTofu),
        runner: || Box::new(meridian_target_opentofu::OpenTofuRunner::from_env()),
        importer: Some(|| Box::new(meridian_target_opentofu::OpenTofuImporter)),
        // OpenTofu describes its types through the Terraform plugin's
        // source, run as `tofu`.
        schemas: Some(|| {
            Box::new(meridian_target_terraform::TerraformSchemas::new(
                "tofu", "opentofu",
            ))
        }),
    },
    target!("openvox", meridian_target_openvox, OpenVox, OpenVoxRunner),
    target!("packer", meridian_target_packer, Packer, PackerRunner),
    target!("podman", meridian_target_podman, Podman, PodmanRunner),
    Target {
        name: "pulumi",
        emitter: || Box::new(meridian_target_pulumi::Pulumi),
        runner: || Box::new(meridian_target_pulumi::PulumiRunner::from_env()),
        importer: None,
        schemas: Some(|| Box::new(meridian_target_pulumi::PulumiSchemas::from_env())),
    },
    target!("puppet", meridian_target_puppet, Puppet, PuppetRunner),
    target!("salt", meridian_target_salt, Salt, SaltRunner),
    Target {
        name: "terraform",
        emitter: || Box::new(meridian_target_terraform::Terraform),
        runner: || Box::new(meridian_target_terraform::TerraformRunner::from_env()),
        importer: Some(|| Box::new(meridian_target_terraform::TerraformImporter)),
        schemas: Some(|| {
            Box::new(meridian_target_terraform::TerraformSchemas::new(
                "terraform",
                "terraform",
            ))
        }),
    },
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
