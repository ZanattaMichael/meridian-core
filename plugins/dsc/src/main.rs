//! The plugin binary: wiring only.

fn main() -> Result<(), meridian_sdk::ServeError> {
    meridian_sdk::serve(
        meridian_sdk::Plugin::new(meridian_target_dsc::Dsc)
            .with_runner(meridian_target_dsc::DscRunner::from_env())
            .with_importer(meridian_target_dsc::DscImporter),
    )
}
