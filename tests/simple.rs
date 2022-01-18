use std::ops::Not;

use lazy_static::lazy_static;
use log::info;
use mx_tester::{self, registration::User, *};

lazy_static! {
    static ref DOCKER: bollard::Docker =
        bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker daemon");
}

/// The version of Synapse to use for testing.
const SYNAPSE_VERSION: &str = "matrixdotorg/synapse:latest";

/// Simple test: empty config.
#[tokio::test]
async fn test_simple() {
    let _ = env_logger::builder().is_test(true).try_init();
    let docker = DOCKER.clone();
    let config = Config::builder()
        .name("test-simple".into())
        .synapse(SynapseVersion::Docker {
            tag: SYNAPSE_VERSION.into(),
        })
        .build();
    mx_tester::build(&docker, &config)
        .await
        .expect("Failed in step `build`");
    mx_tester::up(&docker, &config)
        .await
        .expect("Failed in step `up`");
    mx_tester::down(&docker, &config, Status::Manual)
        .await
        .expect("Failed in step `down`");
}

#[tokio::test]
async fn test_create_users() {
    let _ = env_logger::builder().is_test(true).try_init();

    let docker = DOCKER.clone();

    // Setup with two users.
    let admin = User::builder()
        .admin(true)
        .localname(format!("admin-{}", uuid::Uuid::new_v4()))
        .build();

    let regular_user = User::builder()
        .localname(format!("regular-user-{}", uuid::Uuid::new_v4()))
        .build();
    let regular_user_with_custom_password = User::builder()
        .localname(format!("regular-user-{}", uuid::Uuid::new_v4()))
        .password(format!("{}", uuid::Uuid::new_v4()))
        .build();

    // Use port 9998 to avoid colliding with test_simple.
    let config = Config::builder()
        .name("test-create-users".into())
        .synapse(SynapseVersion::Docker {
            tag: SYNAPSE_VERSION.into(),
        })
        .users(vec![
            admin.clone(),
            regular_user.clone(),
            regular_user_with_custom_password.clone(),
        ])
        .homeserver(
            HomeserverConfig::builder()
                .server_name("localhost:9998".to_string())
                .public_baseurl("http://localhost:9998".to_string())
                .build(),
        )
        .docker(
            DockerConfig::builder()
                .port_mapping(vec![PortMapping {
                    host: 9998,
                    guest: 8008,
                }])
                .build(),
        )
        .build();
    mx_tester::build(&docker, &config)
        .await
        .expect("Failed in step `build`");
    tokio::time::timeout(std::time::Duration::from_secs(1800), async {
        mx_tester::up(&docker, &config)
            .await
            .expect("Failed in step `up`")
    })
    .await
    .expect("Timeout in step `up`");

    // Now attempt to login as our users.
    let homeserver_url = reqwest::Url::parse(&config.homeserver.public_baseurl).unwrap();

    let regular_user_client = matrix_sdk::Client::new(homeserver_url.clone()).unwrap();
    regular_user_client
        .login(&regular_user.localname, &regular_user.password, None, None)
        .await
        .expect("Could not login as regular user");
    let regular_user_id = regular_user_client
        .whoami()
        .await
        .expect("Could not request whoami for regular user")
        .user_id;
    assert!(
        regular_user_id.as_str().contains(&regular_user.localname),
        "Expected to find local name {} in user_id {}",
        regular_user.localname,
        regular_user_id
    );

    let regular_user_client_with_custom_password =
        matrix_sdk::Client::new(homeserver_url.clone()).unwrap();
    regular_user_client_with_custom_password
        .login(
            &regular_user_with_custom_password.localname,
            &regular_user_with_custom_password.password,
            None,
            None,
        )
        .await
        .expect("Could not login as regular user");
    let regular_user_client_with_custom_password_user_id = regular_user_client_with_custom_password
        .whoami()
        .await
        .expect("Could not request whoami for regular user")
        .user_id;
    assert!(
        regular_user_client_with_custom_password_user_id
            .as_str()
            .contains(&regular_user_with_custom_password.localname),
        "Expected to find local name {} in user_id {}",
        regular_user_with_custom_password.localname,
        regular_user_client_with_custom_password_user_id
    );

    let admin_client = matrix_sdk::Client::new(homeserver_url.clone()).unwrap();
    admin_client
        .login(&admin.localname, &admin.password, None, None)
        .await
        .expect("Could not login as admin");
    let admin_user_id = admin_client
        .whoami()
        .await
        .expect("Could not request whoami for admin")
        .user_id;
    assert!(
        admin_user_id.as_str().contains(&admin.localname),
        "Expected to find local name {} in user_id {}",
        admin.localname,
        admin_user_id
    );

    // Now check whether the admin can use the user API and others can't.
    let request = synapse_admin_api::users::get_details::v2::Request::new(&regular_user_id);
    let response = admin_client
        .send(request, None)
        .await
        .expect("Admin could not request user details");
    assert!(response.details.admin.not());
    assert!(response.details.deactivated.not());
    assert_eq!(response.details.displayname, regular_user.localname);

    for client in [
        &regular_user_client,
        &regular_user_client_with_custom_password,
    ] {
        let request = synapse_admin_api::users::get_details::v2::Request::new(&regular_user_id);
        client
            .send(request, None)
            .await
            .expect_err("A non-admin user should not be able to send an admin API request");
    }

    mx_tester::down(&docker, &config, Status::Manual)
        .await
        .expect("Failed in step `down`");
}

/// Simple test: repeat numerous times up/down, to increase the
/// chances of hitting one the cases in which Synapse fails
/// during startup.
#[tokio::test]
async fn test_repeat() {
    let _ = env_logger::builder().is_test(true).try_init();
    let docker = DOCKER.clone();
    let config = Config::builder()
        .name("test-repeat".into())
        .synapse(SynapseVersion::Docker {
            tag: SYNAPSE_VERSION.into(),
        })
        .homeserver(
            HomeserverConfig::builder()
                .server_name("localhost:9997".to_string())
                .public_baseurl("http://localhost:9997".to_string())
                .build(),
        )
        .docker(
            DockerConfig::builder()
                .port_mapping(vec![PortMapping {
                    host: 9997,
                    guest: 8008,
                }])
                .build(),
        )
        .build();
    mx_tester::build(&docker, &config)
        .await
        .expect("Failed in step `build`");
    for i in 0..200 {
        info!("test_repeat: iteration {}", i);
        mx_tester::up(&docker, &config)
            .await
            .expect("Failed in step `up`");
        mx_tester::down(&docker, &config, Status::Manual)
            .await
            .expect("Failed in step `down`");
    }
}
