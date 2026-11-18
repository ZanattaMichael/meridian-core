//! The plugin binary: wiring only.

fn main() -> Result<(), meridian_sdk::ServeError> {
    meridian_sdk::serve(
        meridian_sdk::Plugin::new(meridian_target_openvox::OpenVox)
            .with_runner(meridian_target_openvox::OpenVoxRunner::from_env())
            .with_schema_source(meridian_target_openvox::OpenVoxRunner::from_env().schemas()),
    )
}
