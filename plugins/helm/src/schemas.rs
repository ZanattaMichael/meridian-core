//! Asking the cluster what each kind's fields are, for a chart.
//!
//! A chart's templates are Kubernetes objects, `<apiVersion>/<Kind>` types
//! the API server of the cluster the release goes to describes in its
//! OpenAPI document, exactly as for the kubernetes target. This half of the
//! plugin is the kubernetes plugin's schema source run with this target's
//! `kubectl` settings, asked only about the object types: `chart`,
//! `release`, `values` and `kustomization` are Meridian's own, described
//! by the plugin, and `values` holds whatever the templates read, which no
//! schema describes.

use meridian_sdk::{settings, Log, RunError, SchemaExport, SchemaRequest, SchemaSource};
use meridian_target_kubernetes::KubernetesSchemas;

use crate::runner::settings as runner_settings;

/// Asks the cluster for its OpenAPI document, through this target's
/// `kubectl`.
pub struct HelmSchemas {
    pub kubernetes: KubernetesSchemas,
}

impl HelmSchemas {
    /// The source as the environment configures it: the `kubectl` and
    /// `kubectl-args` settings the runner reads for outputs.
    pub fn from_env() -> Self {
        let declared = runner_settings();
        let setting = |name: &str| settings::find(&declared, name);
        HelmSchemas {
            kubernetes: KubernetesSchemas {
                program: settings::text(setting("kubectl")),
                extra_args: settings::args(setting("kubectl-args")),
            },
        }
    }
}

/// Whether a type is a Kubernetes object the API server can describe.
pub(crate) fn is_object_type(name: &str) -> bool {
    name.contains('/')
}

impl SchemaSource for HelmSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let mut asked = request.clone();
        asked.types.retain(|t| is_object_type(t));
        for graph in &mut asked.graphs {
            graph.resources.retain(|r| is_object_type(&r.r#type));
        }
        let mut export = self.kubernetes.export(&asked, log)?;
        export.tool = "kubectl".to_owned();
        Ok(export)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_kubernetes_objects_are_asked_of_the_cluster() {
        assert!(is_object_type("apps/v1/Deployment") && is_object_type("v1/ConfigMap"));
        assert!(
            !is_object_type("chart")
                && !is_object_type("release")
                && !is_object_type("values")
                && !is_object_type("kustomization")
        );
    }
}
