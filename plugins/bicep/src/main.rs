//! The plugin binary: wiring only.

fn main() -> Result<(), meridian_sdk::ServeError> {
    meridian_sdk::serve(
        meridian_sdk::Plugin::new(meridian_target_bicep::Bicep)
            .with_runner(meridian_target_bicep::BicepRunner::from_env())
            .with_schema_source(meridian_target_bicep::AzureSchemas::new(
                meridian_target_bicep::Dialect::Bicep,
            )),
    )
}
