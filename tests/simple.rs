use std::convert::TryFrom;

use mx_tester::{self, *};

/// A trivial test that checks that steps build, up and down can be executed
/// with the default configuration.
#[tokio::test]
async fn test_default_config() {
    let config = Config::default();
    let container_config = ContainerConfig::try_from(&config)
        .expect("Should be able to convert the config without issue.");
    mx_tester::build(&config.modules, &SynapseVersion::ReleasedDockerImage)
        .expect("Failed in step `build`");
    mx_tester::up(
        &SynapseVersion::ReleasedDockerImage,
        &config,
        &container_config,
        &config.homeserver_config,
    )
    .await
    .expect("Failed in step `up`");
    mx_tester::down(
        &SynapseVersion::ReleasedDockerImage,
        &config.down,
        Status::Manual,
    )
    .expect("Failed in step `down`");
}
