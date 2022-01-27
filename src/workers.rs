//! Tools for setting up Synapse workers.

use anyhow::{Context, Error};
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YAML;

use std::borrow::Cow;

use crate::Config;

#[derive(Deserialize, Serialize, Debug, Clone, Copy, Hash, PartialEq, Eq)]
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
impl WorkerKind {
    fn as_str(&self) -> &'static str {
        match *self {
            WorkerKind::Pusher => "pusher",
            WorkerKind::UserDir => "user_dir",
            WorkerKind::MediaRepository => "media_repository",
            WorkerKind::AppService => "appservice",
            WorkerKind::FederationSender => "federation_sender",
            WorkerKind::FederationReader => "federation_reader",
            WorkerKind::FederationInbound => "federation_inbound",
            WorkerKind::Synchrotron => "synchrotron",
            WorkerKind::EventPersister => "event_persister",
            WorkerKind::BackgroundWorker => "background_worker",
            WorkerKind::EventCreator => "event_creator",
            WorkerKind::FrontendProxy => "frontend_proxy",
        }
    }
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

macro_rules! dict2 {
    // tt muncher for objects
    (@object $factory:expr ; $container:ident ()) => {
        // Empty dict, nothing to insert.
        {}
    };
    (@object $factory:expr ; $container:ident $key:expr => $value:expr) => {
        // Last entry in non-empty dict, no trailing comma.
        let _ = $container.insert($key.into(), $value.into());
    };
    (@object $factory:expr ; $container:ident $key:expr => $value:expr,) => {
        // Last entry in non-empty dict, trailing comma.
        let _ = $container.insert($key.into(), $value.into());
    };

    (@object $factory:expr ; $container:ident $key:expr => { { $($tt:tt)* } }, $($rest:tt)+) => {
        // Non-last entry in non-empty dict, followed by comma - special-cased for sub-dictionaries.
        {
            let _ = $container.insert($key.into(), dict2!($factory, { $($tt)* }));
            dict2!(@object $factory; $($rest)*);
        }
    };
    (@object $factory:expr ; $container:ident $key:expr => $value:expr, $($rest:tt)+) => {
        // Non-last entry in non-empty dict.
        let _ = $container.insert($key.into(), $value.into());
        dict2!(@object $factory; $($rest)*);
    };
    // public-facing API
    ( $factory: expr, { $($tt:tt)+ }) => {
        {
            let mut container = $factory;
            dict2!(@object $factory ; container $($tt)* );
            container
        }
    }
}

fn test() {
    let _ = dict2!(std::collections::HashMap::<String, ()>::new(), { "foo" => {}, });
    let _ = dict2!(std::collections::HashMap::<String, ()>::new(), { "foo" => { "bar" => 5 }, });
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
    let workers_path = config.synapse_root().join("workers");
    std::fs::create_dir_all(&workers_path)
        .context("Could not create directory for worker configuration")?;
 
    // FIXME: supervisord
    // FIXME: nginx
    // FIXME: # Worker-type specific sharding config
    // FIXME: shared.yaml
    // FIXME: Ensure logging directory exists
    // Start worker ports from this arbitrary port.
    const START_WORKER_PORT: usize = 18009;
    // The same worker can be spawned several times.
    let mut counters = std::collections::HashMap::new();
    for (kind, worker_port) in workers.iter().zip(START_WORKER_PORT..) {
        let counter = counters.entry(*kind)
            .and_modify(|i| *i += 1)
            .or_insert(0);
        let name = format!("{name}{counter}",
            name = kind.as_str(),
            counter = counter);

        let log_file_path = workers_path.join(name).as_path().with_extension("log.config")
            .as_os_str()
            .to_str()
            .context("File path cannot be converted to Unicode")?;

        // Generate and write config for this worker.
        let config = worker_config(*kind, config)?;
        let config_file_path = workers_path.join(name).as_path().with_extension(name);
/*
        let config_yaml = dict!(serde_yaml::Mapping::new(), {
            "worker_app" => config.app,
            "worker_name" => name,
            "worker_replication_host" => "127.0.0.1",
            "worker_replication_http_port" => 9093,
            "worker_listeners" => dict!(serde_yaml::Mapping::new(), {vec![

            ]
        });
*/
        let config_content = format!("
# This is a configuration template for a single worker instance, and is
# used by Dockerfile-workers.
# Values will be change depending on whichever workers are selected when
# running that image.

worker_app: \"{app}\"
worker_name: \"{name}\"

# The replication listener on the main synapse process.
worker_replication_host: 127.0.0.1
worker_replication_http_port: 9093

worker_listeners:
  - type: http
    port: {port}
{maybe_listener_resources}

worker_log_config: {worker_log_config_filepath}

{worker_extra_conf}
",
        app = config.app,
        name = name,
        port = worker_port,
        worker_log_config_filepath = log_file_path,
        worker_extra_conf = config.worker_extra_conf,
        maybe_listener_resources = if config.listener_resources.is_empty() {
            Cow::from("")
        } else {
            Cow::from(format!(
"   resources:
        - names:
{}
",
                config.listener_resources.iter().map(|res| format!(
"            - {}
",
                    res
                )).format("")
))
        });
        std::fs::write(config_file_path, config_content)
            .context("Could not write worker configuration")?;

        let log_config_content = format!("
version: 1

formatters:
    precise:
        format: '%(asctime)s - worker:{worker_name} - %(name)s - %(lineno)d - %(levelname)s - %(request)s - %(message)s'

handlers:
    file:
        class: logging.handlers.TimedRotatingFileHandler
        formatter: precise
        filename: {log_file_path}
        when: \"midnight\"
        backupCount: 6  # Does not include the current log file.
        encoding: utf8

    # Default to buffering writes to log file for efficiency.
    # WARNING/ERROR logs will still be flushed immediately, but there will be a
    # delay (of up to `period` seconds, or until the buffer is full with
    # `capacity` messages) before INFO/DEBUG logs get written.
    buffer:
        class: synapse.logging.handlers.PeriodicallyFlushingMemoryHandler
        target: file

        # The capacity is the maximum number of log lines that are buffered
        # before being written to disk. Increasing this will lead to better
        # performance, at the expensive of it taking longer for log lines to
        # be written to disk.
        # This parameter is required.
        capacity: 10

        # Logs with a level at or above the flush level will cause the buffer to
        # be flushed immediately.
        # Default value: 40 (ERROR)
        # Other values: 50 (CRITICAL), 30 (WARNING), 20 (INFO), 10 (DEBUG)
        flushLevel: 30  # Flush immediately for WARNING logs and higher

        # The period of time, in seconds, between forced flushes.
        # Messages will not be delayed for longer than this time.
        # Default value: 5 seconds
        period: 5

    console:
        class: logging.StreamHandler
        formatter: precise

loggers:
    synapse.storage.SQL:
        # beware: increasing this to DEBUG will make synapse log sensitive
        # information such as access tokens.
        level: INFO

root:
    level: {log_level}

    handlers: [console, buffer]

disable_existing_loggers: false        
",
        worker_name = name,
        log_file_path = log_file_path,
        log_level = "INFO");
        std::fs::write(log_file_path, log_config_content)
            .context("Could not write worker logging configuration")?;
    }


    unimplemented!()
}