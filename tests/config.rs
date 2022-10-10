use std::ops::Not;

use log::debug;
use mx_tester::{cleanup::Cleanup, Config, Status};

mod shared;
use shared::{AssignPort, DOCKER};

const LARGE_VALUE: i64 = 1_000_000_000;

/// Simple test: empty config.
#[tokio::test(flavor = "multi_thread")]
async fn test_default_rate_limit() {
    let _ = env_logger::builder().is_test(true).try_init();
    let docker = DOCKER.clone();

    let config: Config = serde_yaml::from_str::<'_, Config>("name: \"default-rate-limit\"")
        .expect("Invalid config file")
        .assign_port();

    let mut content = serde_yaml::Mapping::new();
    config
        .patch_homeserver_config_content(&mut content)
        .unwrap();

    // Test expected values.
    for (category, maybe_subcategory) in &[
        ("rc_message", None),
        ("rc_registration", None),
        ("rc_admin_redaction", None),
        ("rc_invites", Some("per_room")),
        ("rc_invites", Some("per_user")),
        ("rc_invites", Some("per_sender")),
    ] {
        for limit in &["per_second", "burst_count"] {
            let per_category = content
                .get(category)
                .unwrap_or_else(|| panic!("Missing {}", category))
                .as_mapping()
                .unwrap_or_else(|| panic!("Invalid {}", category));
            let (name, mapping) = match maybe_subcategory {
                None => (category.to_string(), per_category),
                Some(sub) => (
                    format!("{}.{}", category, sub),
                    per_category
                        .get(sub)
                        .unwrap_or_else(|| panic!("Missing {}.{}", category, sub))
                        .as_mapping()
                        .unwrap_or_else(|| panic!("Invalid {}.{}", category, sub)),
                ),
            };
            let value = mapping
                .get(limit)
                .unwrap_or_else(|| panic!("Missing {}.{}", name, limit))
                .as_i64()
                .unwrap_or_else(|| panic!("Invalid or missing {}.{}", name, limit));
            assert_eq!(value, LARGE_VALUE, "Invalid {}.{}", name, limit);
        }
    }

    // Test that Synapse can launch with this configuration
    let mut _guard = Cleanup::new(&config);
    _guard.cleanup_network(true);
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

/// Simple test: override rate config with synapse-default.
#[tokio::test(flavor = "multi_thread")]
async fn test_synapse_provides_rate_limit() {
    let _ = env_logger::builder().is_test(true).try_init();
    let docker = DOCKER.clone();

    const CATEGORIES: &[(&str, Option<&str>)] = &[
        ("rc_message", None),
        ("rc_registration", None),
        ("rc_admin_redaction", None),
        ("rc_login", Some("address")),
        ("rc_login", Some("account")),
        ("rc_login", Some("failed_attempts")),
        ("rc_invites", Some("per_room")),
        ("rc_invites", Some("per_user")),
        ("rc_invites", Some("per_sender")),
    ];
    for (patch_category, patch_maybe_subcategory) in CATEGORIES {
        debug!(
            "Running with patch {}-{:?}",
            patch_category, patch_maybe_subcategory
        );
        let test_name = match patch_maybe_subcategory {
            None => format!("test-synapse-default-{}", patch_category),
            Some(sub) => format!("test-synapse-default-{}-{}", patch_category, sub),
        };
        let mut config: Config =
            serde_yaml::from_str::<'_, Config>(&format!("name: '{}'", test_name))
                .expect("Invalid config file")
                .assign_port();
        config
            .homeserver
            .extra_fields
            .insert((*patch_category).into(), "synapse-default".into());

        let mut content = serde_yaml::Mapping::new();
        config
            .patch_homeserver_config_content(&mut content)
            .unwrap();
        // Test that the value is as expected.
        for (category, maybe_subcategory) in CATEGORIES {
            if patch_category == category {
                assert!(
                    content.contains_key(category).not(),
                    "Key {} should be absent in homeserver.yaml",
                    category
                );
                continue;
            }
            for limit in &["per_second", "burst_count"] {
                let per_category = content
                    .get(category)
                    .unwrap_or_else(|| panic!("Missing {}", category))
                    .as_mapping()
                    .unwrap_or_else(|| panic!("Invalid {}", category));
                let (name, mapping) = match maybe_subcategory {
                    None => (category.to_string(), per_category),
                    Some(sub) => (
                        format!("{}.{}", category, sub),
                        per_category
                            .get(sub)
                            .unwrap_or_else(|| panic!("Missing {}.{}", category, sub))
                            .as_mapping()
                            .unwrap_or_else(|| panic!("Invalid {}.{}", category, sub)),
                    ),
                };
                let value = mapping
                    .get(limit)
                    .unwrap_or_else(|| panic!("Missing {}.{}", name, limit))
                    .as_i64()
                    .unwrap_or_else(|| panic!("Invalid or missing {}.{}", name, limit));
                if category == patch_category && maybe_subcategory == patch_maybe_subcategory {
                    assert_ne!(
                        value, LARGE_VALUE,
                        "Invalid {}.{} (should be Synapse default)",
                        name, limit
                    );
                } else {
                    assert_eq!(
                        value, LARGE_VALUE,
                        "Invalid {}.{} (should be mx-tester default)",
                        name, limit
                    );
                }
            }
        }

        // Test that Synapse can launch with this configuration
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
}
