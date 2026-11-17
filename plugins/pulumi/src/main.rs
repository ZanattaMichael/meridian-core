//! The plugin binary: wiring only.

fn main() -> Result<(), meridian_sdk::ServeError> {
    meridian_sdk::serve(
        meridian_sdk::Plugin::new(meridian_target_pulumi::Pulumi)
            .with_runner(meridian_target_pulumi::PulumiRunner::from_env())
            .with_schema_source(meridian_target_pulumi::PulumiSchemas::from_env()),
    )
}
