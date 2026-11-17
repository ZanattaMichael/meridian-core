//! The plugin binary: wiring only.

fn main() -> Result<(), meridian_sdk::ServeError> {
    meridian_sdk::serve(
        meridian_sdk::Plugin::new(meridian_target_kubernetes::Kubernetes)
            .with_runner(meridian_target_kubernetes::KubernetesRunner::from_env())
            .with_importer(meridian_target_kubernetes::KubernetesImporter),
    )
}
