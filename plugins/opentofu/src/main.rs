//! The plugin binary: wiring only.

fn main() -> Result<(), meridian_sdk::ServeError> {
    meridian_sdk::serve(
        meridian_sdk::Plugin::new(meridian_target_opentofu::OpenTofu)
            .with_runner(meridian_target_opentofu::OpenTofuRunner::from_env())
            .with_importer(meridian_target_opentofu::OpenTofuImporter),
    )
}
