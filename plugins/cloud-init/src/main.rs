//! The plugin binary: wiring only.

fn main() -> Result<(), meridian_sdk::ServeError> {
    meridian_sdk::serve(
        meridian_sdk::Plugin::new(meridian_target_cloud_init::CloudInit)
            .with_runner(meridian_target_cloud_init::CloudInitRunner::from_env())
            .with_schema_source(meridian_target_cloud_init::CloudInitSchemas::from_env()),
    )
}
