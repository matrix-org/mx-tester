//! A number of simple tests (i.e. modules are not tested) on mx-tester.
//!
//! Each test needs to use #[tokio::test(flavor = "multi_thread")], as this
//! is needed for auto-cleanup in case of failure.

use std::ops::Not;

use lazy_static::lazy_static;
use log::{debug, info};
use mx_tester::{self, cleanup::Cleanup, registration::User, *};

lazy_static! {
    static ref DOCKER: bollard::Docker =
        bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker daemon");
}

/// The version of Synapse to use for testing.
const SYNAPSE_VERSION: &str = "matrixdotorg/synapse:latest";

/// Utility trait, designed to simplify assigning a random port for a test.
trait AssignPort {
    /// Assign a random port for a test.
    fn assign_port(self) -> Self;
}

impl AssignPort for Config {
    /// Assign a random port for a test.
    fn assign_port(mut self) -> Self {
        use rand::Rng;
        let port = loop {
            let port = rand::thread_rng().gen_range(1025..10_000);
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                debug!("This test will use port {}", port);
                break port as u64;
            }
            debug!("Port {} already occupied, looking for another", port);
        };
        self.homeserver.set_host_port(port);
        self
    }
}

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
