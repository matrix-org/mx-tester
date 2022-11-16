//! A number of simple tests (i.e. modules are not tested) on mx-tester.
//!
//! Each test needs to use #[tokio::test(flavor = "multi_thread")], as this
//! is needed for auto-cleanup in case of failure.

use std::{collections::HashMap, ops::Not};

use anyhow::Context;
use log::info;
use mx_tester::{self, cleanup::Cleanup, registration::User, *};

mod shared;
use shared::{AssignPort, DOCKER};

/// The version of Synapse to use for testing.
const SYNAPSE_VERSION: &str = "matrixdotorg/synapse:latest";

/// Simple test: empty config.
#[tokio::test(flavor = "multi_thread")]
async fn test_simple() {
    let _ = env_logger::builder().is_test(true).try_init();
    let docker = DOCKER.clone();
    let config = Config::builder()
        .name("test-simple".into())
        .synapse(SynapseVersion::Docker {
            tag: SYNAPSE_VERSION.into(),
        })
        .build()
        .assign_port();
    let _ = Cleanup::new(&config);
    mx_tester::build(&docker, &config)
        .await
        .expect("Failed in step `build`");
    mx_tester::up(&docker, &config)
        .await
        .expect("Failed in step `up`");
    let response = reqwest::get(format!(
        "http://localhost:{port}/health",
        port = config.homeserver.host_port
    ))
    .await
    .expect("Could not get /health")
    .text()
    .await
    .expect("Invalid /health");
    assert_eq!(response, "OK");
    mx_tester::down(&docker, &config, Status::Manual)
        .await
        .expect("Failed in step `down`");
}

#[tokio::test(flavor = "multi_thread")]

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

    let config = Config::builder()
        .name("test-create-users".into())
        .users(vec![
            admin.clone(),
            regular_user.clone(),
            regular_user_with_custom_password.clone(),
        ])
        .build()
        .assign_port();
    let _ = Cleanup::new(&config);
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

    let regular_user_client = matrix_sdk::Client::new(homeserver_url.clone())
        .await
        .unwrap();
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

    let regular_user_client_with_custom_password = matrix_sdk::Client::new(homeserver_url.clone())
        .await
        .unwrap();
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

    let admin_client = matrix_sdk::Client::new(homeserver_url.clone())
        .await
        .unwrap();
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
#[tokio::test(flavor = "multi_thread")]

async fn test_repeat() {
    let _ = env_logger::builder().is_test(true).try_init();
    let docker = DOCKER.clone();
    let config = Config::builder()
        .name("test-repeat".into())
        .synapse(SynapseVersion::Docker {
            tag: SYNAPSE_VERSION.into(),
        })
        .build()
        .assign_port();
    let _ = Cleanup::new(&config);
    mx_tester::build(&docker, &config)
        .await
        .expect("Failed in step `build`");
    for i in 0..20 {
        info!("test_repeat: iteration {}", i);
        mx_tester::up(&docker, &config)
            .await
            .expect("Failed in step `up`");
        let response = reqwest::get(format!(
            "http://localhost:{port}/health",
            port = config.homeserver.host_port
        ))
        .await
        .expect("Could not get /health")
        .text()
        .await
        .expect("Invalid /health");
        assert_eq!(response, "OK");
        mx_tester::down(&docker, &config, Status::Manual)
            .await
            .expect("Failed in step `down`");
    }
}

/// Simple test: repeat numerous times up/down, to increase the
/// chances of hitting one the cases in which Synapse fails
/// during startup.
#[tokio::test(flavor = "multi_thread")]
async fn test_empty_appservice() {
    let docker = DOCKER.clone();
    let config = Config::builder()
        .name("test-appservice".into())
        .synapse(SynapseVersion::Docker {
            tag: SYNAPSE_VERSION.into(),
        })
        .appservices(
            AllAppservicesConfig::builder()
                .host(vec![AppServiceConfig::builder()
                    .name("some-appservice".into())
                    .url("http://host:8888".parse().unwrap())
                    .sender_localpart("_ghost".into())
                    .extra_fields(dict!(HashMap::new(), { "another-field" => 0 }))
                    .build()])
                .build(),
        )
        .build()
        .assign_port();
    let _ = Cleanup::new(&config);
    mx_tester::build(&docker, &config)
        .await
        .expect("Failed in step `build`");
    mx_tester::up(&docker, &config).await.expect_err(
        "Step `up` should not be able to complete as we don't really have an appservice",
    );
    let path = config.generated_appservice_path("some-appservice");
    let generated = std::fs::read_to_string(&path)
        .with_context(|| format!("Could not read appservice file at {:?}", path))
        .unwrap();
    let yaml: serde_yaml::Value = serde_yaml::from_str(&generated)
        .with_context(|| format!("Could not parse appservice file at {:?}", path))
        .unwrap();
    assert_eq!(
        yaml["url"],
        serde_yaml::Value::from("http://localhost:8888/"),
    );
    assert_eq!(yaml["another-field"], serde_yaml::Value::from(0));
}

/*
/// Simple test: spawn workers, do nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn test_workers() {
    let _ = env_logger::builder().is_test(true).try_init();
    let docker = DOCKER.clone();
    let config = Config::builder()
        .name("test-simple-workers".into())
        .workers(WorkersConfig::builder().enabled(true).build())
        .build()
        .assign_port();
    let _ = Cleanup::new(&config);
    mx_tester::build(&docker, &config)
        .await
        .expect("Failed in step `build`");
    mx_tester::up(&docker, &config)
        .await
        .expect("Failed in step `up`");
    'wait_for_health: loop {
        // For this version, it looks like nginx isn't forwarding `/health` anywhere,
        // so let's go for another well-known URL.
        #[derive(Deserialize)]
        struct Versions {
            versions: Vec<String>,
        }
        let response = reqwest::get(format!(
            "http://localhost:{port}/_matrix/client/versions",
            port = config.homeserver.host_port
        ))
        .await
        .expect("Could not get /_matrix/client/versions");
        let text = response
            .text()
            .await
            .expect("Garbled /_matrix/client/versions");
        if let Ok(versions) = serde_json::from_str(&text) {
            let _: &Versions = &versions;
            debug!("Found version {:?}", versions.versions);
            break 'wait_for_health;
        }
        eprintln!("RESPONSE: {:?}", text);
        debug!("Received unexpected response: {:?}", text);
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
    mx_tester::down(&docker, &config, Status::Manual)
        .await
        .expect("Failed in step `down`");
}
 */
