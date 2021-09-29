use mx_tester::{self, *};

/// A trivial test that checks that steps build, up and down can be executed
/// with the default configuration.
#[test]
fn test_default_config() {
    let config = Config::default();
    let container_config = ContainerConfig::from_mx_tester_config(&config);
    mx_tester::build(&config.modules, &SynapseVersion::ReleasedDockerImage)
        .expect("Failed in step `build`");
    mx_tester::up(
        &SynapseVersion::ReleasedDockerImage,
        &config.up,
        &container_config,
        &config.homeserver_config,
    )
    .expect("Failed in step `up`");
    mx_tester::down(
        &SynapseVersion::ReleasedDockerImage,
        &config.down,
        Status::Manual,
    )
    .expect("Failed in step `down`");
}
