//! Tools for setting up Synapse workers.

use anyhow::{Context, Error};
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YAML;

use std::borrow::Cow;

use crate::Config;

#[derive(Deserialize, Serialize, Debug, Clone, Copy)]
enum WorkerKind {
    #[serde(rename="pusher")]
    Pusher,
    #[serde(rename="user_dir")]
    UserDir,
    #[serde(rename="media_repository")]
    MediaRepository,
    #[serde(rename="appservice")]
    AppService,
    #[serde(rename="federation_sender")]
    FederationSender,
    #[serde(rename="federation_reader")]
    FederationReader,
    #[serde(rename="federation_inbound")]
    FederationInbound,
    #[serde(rename="synchrotron")]
    Synchrotron,
    #[serde(rename="event_persister")]
    EventPersister,
    #[serde(rename="background_worker")]
    BackgroundWorker,
    #[serde(rename="event_creator")]
    EventCreator,
    #[serde(rename="frontend_proxy")]
    FrontendProxy,
}

/// A generic syntax for dict-like structures.
///
/// Works for HashMap but also for serde_json or serde_yaml maps.
///
/// ```rust
/// use std::collections::HashMap;
///
/// let empty: HashMap<u8, u8> = dict!(HashMap::new(), {});
/// assert_eq!(empty.len(), 0);
///
/// let map: HashMap<u8, u8> = dict!(HashMap::new(), {
///    0 => 255,
///    1 => 254,
///    2 => 253,
/// });
/// assert_eq!(map.len(), 3);
/// assert(matches!(map.get(0), Some(255)));
/// assert(matches!(map.get(1), Some(254)));
/// assert(matches!(map.get(2), Some(253)));
/// ```
macro_rules! dict {
    // Empty
    ( $container: expr, {}) => {
        $container
    };
    // Without trailing `,`.
    ( $container: expr, { $( $k:expr => $v:expr ),+ } ) => {
        dict!($container, { $($k => $v,)* })
    };
    // With trailing `,`.
    ( $container: expr, { $( $k:expr => $v:expr ),+, } ) => {
        {
            let mut container = $container;
            $(
                container.insert($k.into(), $v.into());
            )*
            container
        }
    };
}

pub fn replication_listener() -> YAML {
    dict!(serde_yaml::Mapping::new(), {
        "port" => 9093,
        "bind_address" => "127.0.0.1",
        "type" => "http",
        "resources" => vec![
            dict!(serde_yaml::Mapping::new(), {
                "names" => vec!["replication"]
            }
        )]
    }).into()
}

#[derive(Default, Serialize)]
struct WorkerData {
    app: Cow<'static, str>,
    listener_resources: Vec<Cow<'static, str>>,
    endpoint_patterns:  Vec<Cow<'static, str>>,
    shared_extra_conf: YAML,
    worker_extra_conf: Cow<'static, str>,
}

// Adapted from Synapse's `configure_workers_and_start.py`.
fn worker_config(worker: WorkerKind, config: &crate::Config) -> Result<WorkerData, Error> {
    use WorkerKind::*;
    let config = match worker {
        Pusher => WorkerData {
            app: "synapse.app.pusher".into(),
            shared_extra_conf: dict!(serde_yaml::Mapping::new(), {"start_pushers" => false}).into(),
            ..WorkerData::default()
        },
        UserDir => WorkerData {
            app: "synapse.app.user_dir".into(),
            listener_resources: vec!["client".into()],
            endpoint_patterns: vec![
                "^/_matrix/client/(api/v1|r0|v3|unstable)/user_directory/search$".into()
            ],
            shared_extra_conf: dict!(serde_yaml::Mapping::new(), {"update_user_directory" => false}).into(),
            ..WorkerData::default()
        },
        MediaRepository => WorkerData {
            app: "synapse.app.media_repository".into(),
            listener_resources: vec!["media".into()],
            endpoint_patterns: vec![
                "^/_matrix/media/".into(),
                "^/_synapse/admin/v1/purge_media_cache$".into(),
                "^/_synapse/admin/v1/room/.*/media.*$".into(),
                "^/_synapse/admin/v1/user/.*/media.*$".into(),
                "^/_synapse/admin/v1/media/.*$".into(),
                "^/_synapse/admin/v1/quarantine_media/.*$".into(),
            ],
            shared_extra_conf: dict!(serde_yaml::Mapping::new(), {"enable_media_repo" => false}).into(),
            worker_extra_conf: "enable_media_repo: true".into(),
        },
        AppService => WorkerData {
            app: "synapse.app.appservice".into(),
            shared_extra_conf: dict!(serde_yaml::Mapping::new(), {"notify_appservices" => false}).into(),
            ..WorkerData::default()
        },
        FederationSender => WorkerData {
            app: "synapse.app.federation_sender".into(),
            shared_extra_conf: dict!(serde_yaml::Mapping::new(), {"send_federation" => false}).into(),
            ..WorkerData::default()
        },
        FederationReader => WorkerData {
            app: "synapse.app.generic_worker".into(),
            listener_resources: vec!["federation".into()],
            endpoint_patterns: vec![
                "^/_matrix/federation/(v1|v2)/event/".into(),
                "^/_matrix/federation/(v1|v2)/state/".into(),
                "^/_matrix/federation/(v1|v2)/state_ids/".into(),
                "^/_matrix/federation/(v1|v2)/backfill/".into(),
                "^/_matrix/federation/(v1|v2)/get_missing_events/".into(),
                "^/_matrix/federation/(v1|v2)/publicRooms".into(),
                "^/_matrix/federation/(v1|v2)/query/".into(),
                "^/_matrix/federation/(v1|v2)/make_join/".into(),
                "^/_matrix/federation/(v1|v2)/make_leave/".into(),
                "^/_matrix/federation/(v1|v2)/send_join/".into(),
                "^/_matrix/federation/(v1|v2)/send_leave/".into(),
                "^/_matrix/federation/(v1|v2)/invite/".into(),
                "^/_matrix/federation/(v1|v2)/query_auth/".into(),
                "^/_matrix/federation/(v1|v2)/event_auth/".into(),
                "^/_matrix/federation/(v1|v2)/exchange_third_party_invite/".into(),
                "^/_matrix/federation/(v1|v2)/user/devices/".into(),
                "^/_matrix/federation/(v1|v2)/get_groups_publicised$".into(),
                "^/_matrix/key/v2/query".into(),
            ],
            ..WorkerData::default()
        },
        FederationInbound => WorkerData {
            app: "synapse.app.generic_worker".into(),
            listener_resources: vec!["federation".into()],
            endpoint_patterns: vec!["/_matrix/federation/(v1|v2)/send/".into()],
            ..WorkerData::default()
        },
        Synchrotron => WorkerData {
            app: "synapse.app.generic_worker".into(),
            listener_resources: vec!["client".into()],
            endpoint_patterns: vec![
                "^/_matrix/client/(v2_alpha|r0|v3)/sync$".into(),
                "^/_matrix/client/(api/v1|v2_alpha|r0|v3)/events$".into(),
                "^/_matrix/client/(api/v1|r0|v3)/initialSync$".into(),
                "^/_matrix/client/(api/v1|r0|v3)/rooms/[^/]+/initialSync$".into(),
            ],
            ..WorkerData::default()
        },
        EventPersister => WorkerData {
            app: "synapse.app.generic_worker".into(),
            listener_resources: vec!["replication".into()],
            ..WorkerData::default()
        },
        BackgroundWorker => WorkerData {
            app: "synapse.app.generic_worker".into(),
            // This worker cannot be sharded. Therefore there should only ever be one background
            // worker, and it should be named background_worker1
            shared_extra_conf: dict!(serde_yaml::Mapping::new(), {"run_background_tasks_on" => "background_worker1"}).into(),
            ..WorkerData::default()
        },
        EventCreator => WorkerData {
            app: "synapse.app.generic_worker".into(),
            listener_resources: vec!["client".into()],
            endpoint_patterns: vec![
                "^/_matrix/client/(api/v1|r0|v3|unstable)/rooms/.*/redact".into(),
                "^/_matrix/client/(api/v1|r0|v3|unstable)/rooms/.*/send".into(),
                "^/_matrix/client/(api/v1|r0|v3|unstable)/rooms/.*/(join|invite|leave|ban|unban|kick)$".into(),
                "^/_matrix/client/(api/v1|r0|v3|unstable)/join/".into(),
                "^/_matrix/client/(api/v1|r0|v3|unstable)/profile/".into(),
            ],
            ..WorkerData::default()
        },
        FrontendProxy => WorkerData {
            app: "synapse.app.frontend_proxy".into(),
            listener_resources: vec!["client".into(), "replication".into()],
            endpoint_patterns: vec!["^/_matrix/client/(api/v1|r0|v3|unstable)/keys/upload".into()],
            worker_extra_conf:
                format!("worker_main_http_uri: http://127.0.0.1:{}", config.docker.guest_port().context("No guest port specified")?).into(),
            ..WorkerData::default()
        },
    };
    Ok(config.into())
}

fn generate_workers_config(config: &Config, workers: &[WorkerKind]) -> Result<(), Error> {
    
}