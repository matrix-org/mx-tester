// Copyright 2021 The Matrix.org Foundation C.I.C.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod cleanup;
pub mod exec;
pub mod registration;
mod util;

use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{anyhow, Context, Error};
use bollard::{
    auth::DockerCredentials,
    container::{
        Config as BollardContainerConfig, CreateContainerOptions, ListContainersOptions,
        LogsOptions, StartContainerOptions, WaitContainerOptions,
    },
    exec::{CreateExecOptions, StartExecOptions},
    models::{
        EndpointSettings, HostConfig, HostConfigLogConfig, PortBinding, RestartPolicy,
        RestartPolicyNameEnum,
    },
    network::{ConnectNetworkOptions, CreateNetworkOptions, ListNetworksOptions},
    Docker,
};
use cleanup::{Cleanup, Disarm};
use futures_util::stream::StreamExt;
use itertools::Itertools;
use lazy_static::lazy_static;
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio_util::codec::{BytesCodec, FramedRead};
use typed_builder::TypedBuilder;

use registration::{handle_user_registration, User};

use crate::{
    exec::{CommandExt, Executor},
    util::YamlExt,
};

lazy_static! {
    /// Environment variable: the directory where a given module should be copied.
    ///
    /// Passed to `build` scripts.
    static ref MX_TEST_MODULE_DIR: OsString = OsString::from_str("MX_TEST_MODULE_DIR").unwrap();

    /// Environment variable: the directory where the synapse modules are placed in sub directories.
    ///
    /// Passed to `build` scripts.
    static ref MX_TEST_SYNAPSE_DIR: OsString = OsString::from_str("MX_TEST_SYNAPSE_DIR").unwrap();

    /// Environment variable: a temporary directory where scripts can store data.
    ///
    /// Passed to `build`, `up`, `run`, `down` scripts.
    static ref MX_TEST_SCRIPT_TMPDIR: OsString = OsString::from_str("MX_TEST_SCRIPT_TMPDIR").unwrap();

    /// Environment variable: the directory where we launched the test.
    ///
    /// Passed to `build`, `up`, `run`, `down` scripts.
    static ref MX_TEST_CWD: OsString = OsString::from_str("MX_TEST_CWD").unwrap();

    /// Environment variable: defined if workers are enabled.
    ///
    /// Passed to `build`, `up`, `run`, `down` scripts.
    static ref MX_TEST_WORKERS_ENABLED: OsString = OsString::from_str("MX_TEST_WORKERS_ENABLED").unwrap();

    /// Environment variable: the name of the network.
    ///
    /// Passed to `build`, `up`, `run`, `down` scripts.
    static ref MX_TEST_NETWORK_NAME: OsString = OsString::from_str("MX_TEST_NETWORK_NAME").unwrap();

    /// Environment variable: the name of the container used during the build phase.
    ///
    /// Passed to `build`, `up`, `run`, `down` scripts.
    static ref MX_TEST_SETUP_CONTAINER_NAME: OsString = OsString::from_str("MX_TEST_SETUP_CONTAINER_NAME").unwrap();

    /// Environment variable: the name of the container used during the up, run, down phases.
    ///
    /// Passed to `build`, `up`, `run`, `down` scripts.
    static ref MX_TEST_UP_RUN_DOWN_CONTAINER_NAME: OsString = OsString::from_str("MX_TEST_UP_RUN_DOWN_CONTAINER_NAME").unwrap();
}

/// The amount of memory to allocate
const MEMORY_ALLOCATION_BYTES: i64 = 4 * 1024 * 1024 * 1024;

/// The maximal number of times we can restart Synapse in case it stops accidentally.
///
/// Accidental stops are typically due:
/// 1. to Synapse not being able to open its port at startup (this happens, for reasons unknown);
/// 2. to Synapse receiving a SIGTERM (this happens, for reasons unknown);
/// 3. to a synax error or startup error in a module.
const MAX_SYNAPSE_RESTART_COUNT: i64 = 20;

/// The port used by the homeserver inside Docker.
///
/// In single process mode, that's the port used by Synapse.
/// In worker mode, that's the port used by nginx as a load-balancer.
const HARDCODED_GUEST_PORT: u64 = 8008;

/// In worker mode, the port used by the homeserver for the main process
/// inside Docker.
const HARDCODED_MAIN_PROCESS_HTTP_LISTENER_PORT: u64 = 8080;

const TIMEOUT_USER_REGISTRATION_SIMPLE: std::time::Duration = std::time::Duration::new(120, 0);

/// A port in the container made accessible on the host machine.
#[derive(Clone, Debug, Deserialize)]
pub struct PortMapping {
    /// The port, as visible on the host machine.
    pub host: u64,

    /// The port, as visible on the guest, i.e. in the container.
    pub guest: u64,
}

/// Docker-specific configuration to use in the test.
#[derive(Debug, Deserialize, TypedBuilder)]
pub struct DockerConfig {
    /// The hostname to give the synapse container on the docker network, if the docker network has been provided.
    /// Defaults to `synapse` but will not be used unless a network is provided in network.
    #[serde(default = "DockerConfig::default_hostname")]
    #[builder(default = DockerConfig::default_hostname())]
    pub hostname: String,

    /// The docker port mapping configuration to use for the synapse container.
    ///
    /// When generating the Docker image and the Synapse configuration,
    /// we automatically add a mapping HARDCODED_GUEST_PORT -> `homeserver_config.host_port`.
    #[serde(default)]
    #[builder(default = vec![])]
    pub port_mapping: Vec<PortMapping>,
}

impl Default for DockerConfig {
    fn default() -> DockerConfig {
        Self::builder().build()
    }
}

impl DockerConfig {
    fn default_hostname() -> String {
        "synapse".to_string()
    }
}

/// Configuration for the homeserver.
///
/// This will be applied to homeserver.yaml.
#[derive(Debug, Deserialize, Serialize, TypedBuilder)]
pub struct HomeserverConfig {
    /// The port exposed on the host
    #[serde(default = "HomeserverConfig::host_port_default")]
    #[builder(default = HomeserverConfig::host_port_default())]
    pub host_port: u64,

    /// The name of the homeserver.
    #[serde(default = "HomeserverConfig::server_name_default")]
    #[builder(default = HomeserverConfig::server_name_default())]
    pub server_name: String,

    /// The URL to communicate to the server with.
    #[serde(default = "HomeserverConfig::public_baseurl_default")]
    #[builder(default = HomeserverConfig::public_baseurl_default())]
    pub public_baseurl: String,

    #[serde(default = "HomeserverConfig::registration_shared_secret_default")]
    #[builder(default = HomeserverConfig::registration_shared_secret_default())]
    /// The registration shared secret, if provided.
    pub registration_shared_secret: String,

    #[serde(flatten)]
    #[builder(default)]
    /// Any extra fields in the homeserver config
    pub extra_fields: HashMap<String, serde_yaml::Value>,
}

impl Default for HomeserverConfig {
    fn default() -> HomeserverConfig {
        Self::builder().build()
    }
}

impl HomeserverConfig {
    /// Set the port, resetting the public base url.
    pub fn set_host_port(&mut self, port: u64) {
        self.host_port = port;
        self.server_name = format!("localhost:{}", port);
        self.public_baseurl = format!("http://localhost:{}", port);
    }
    pub fn host_port_default() -> u64 {
        9999
    }
    pub fn server_name_default() -> String {
        "localhost:9999".to_string()
    }
    pub fn public_baseurl_default() -> String {
        format!("http://{}", Self::server_name_default())
    }
    pub fn registration_shared_secret_default() -> String {
        "MX_TESTER_REGISTRATION_DEFAULT".to_string()
    }
}

/// Configuring workers
#[derive(Debug, TypedBuilder, Deserialize)]
pub struct WorkersConfig {
    #[serde(default)]
    #[builder(default = false)]
    pub enabled: bool,
}
impl Default for WorkersConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// The contents of a mx-tester.yaml
#[derive(Debug, TypedBuilder, Deserialize)]
pub struct Config {
    /// A name for this test.
    ///
    /// This name is used to generate images, Docker networks, etc.
    pub name: String,

    /// Modules to install in Synapse.
    #[serde(default)]
    #[builder(default)]
    pub modules: Vec<ModuleConfig>,

    #[serde(default)]
    #[builder(default)]
    /// Values to pass through into the homeserver.yaml for this synapse.
    pub homeserver: HomeserverConfig,

    #[serde(default)]
    #[builder(default)]
    /// A script to run at the end of the setup phase.
    pub up: Option<UpScript>,

    #[serde(default)]
    #[builder(default)]
    /// The testing script to run.
    pub run: Option<Script>,

    #[serde(default)]
    #[builder(default)]
    /// A script to run at the start of the teardown phase.
    pub down: Option<DownScript>,

    #[serde(default)]
    #[builder(default)]
    /// Configuration for the docker network.
    pub docker: DockerConfig,

    #[serde(default)]
    #[builder(default)]
    /// Any users to register and make available
    pub users: Vec<User>,

    #[serde(default)]
    #[builder(default)]
    /// The version of Synapse to use
    pub synapse: SynapseVersion,

    #[serde(default)]
    #[builder(default)]
    /// Information for logging to a registry.
    ///
    /// May be overridden from the command-line.
    pub credentials: DockerCredentials,

    #[serde(default)]
    #[builder(default)]
    /// Directories to use for the test.
    ///
    /// May be overridden from the command-line.
    pub directories: Directories,

    #[serde(default)]
    #[builder(default)]
    /// Specify whether workers should be used.
    ///
    /// May be overridden from the command-line.
    pub workers: WorkersConfig,

    #[serde(default = "util::true_")]
    #[builder(default = true)]
    /// Specify whether workers should be used.
    ///
    /// May be overridden from the command-line.
    pub autoclean_on_error: bool,
}

impl Config {
    /// Create a map containing the environment variables that are common
    /// to all scripts.
    ///
    /// Callers may add additional variables that are specific to a given
    /// script step.
    pub fn shared_env_variables(&self) -> Result<HashMap<&'static OsStr, OsString>, Error> {
        let synapse_root = self.synapse_root();
        let script_tmpdir = synapse_root.join("scripts");
        std::fs::create_dir_all(&script_tmpdir)
            .with_context(|| format!("Could not create directory {:#?}", script_tmpdir,))?;
        let curdir = std::env::current_dir()?;
        let env: HashMap<&'static OsStr, OsString> = std::iter::IntoIterator::into_iter([
            (
                MX_TEST_SYNAPSE_DIR.as_os_str(),
                synapse_root.as_os_str().into(),
            ),
            (
                MX_TEST_SCRIPT_TMPDIR.as_os_str(),
                script_tmpdir.as_os_str().into(),
            ),
            (MX_TEST_CWD.as_os_str(), curdir.as_os_str().into()),
            (MX_TEST_NETWORK_NAME.as_os_str(), self.network().into()),
            (
                MX_TEST_SETUP_CONTAINER_NAME.as_os_str(),
                self.setup_container_name().into(),
            ),
            (
                MX_TEST_UP_RUN_DOWN_CONTAINER_NAME.as_os_str(),
                self.run_container_name().into(),
            ),
        ])
        .chain(
            if self.workers.enabled {
                Some((MX_TEST_WORKERS_ENABLED.as_os_str(), "true".into()))
            } else {
                None
            }
            .into_iter(),
        )
        .collect();
        Ok(env)
    }

    /// Patch the homeserver.yaml at the given path (usually one that has been generated by synapse)
    /// with the properties in this struct (which will usually have been provided from mx-tester.yaml)
    ///
    /// In multiple workers mode, also patch the worker files.
    pub fn patch_homeserver_config(&self) -> Result<(), Error> {
        use serde_yaml::Mapping;
        let target_path = self.synapse_root().join("data").join("homeserver.yaml");
        debug!("Attempting to open {:#?}", target_path);
        let config_file = std::fs::File::open(&target_path)
            .context("Could not open the homeserver.yaml generated by synapse")?;
        let mut config: Mapping = serde_yaml::from_reader(config_file)
            .context("The homeserver.yaml generated by synapse is invalid")?;
        self.patch_homeserver_config_content(&mut config)?;
        serde_yaml::to_writer(std::fs::File::create(&target_path)?, &config)
            .context("Could not write combined homeserver config")?;
        Ok(())
    }
    pub fn patch_homeserver_config_content(
        &self,
        config: &mut serde_yaml::Mapping,
    ) -> Result<(), Error> {
        use serde_yaml::Value as YAML;
        const LISTENERS: &str = "listeners";
        const MODULES: &str = "modules";
        let combined_config = config;

        for (key, value) in [
            ("public_baseurl", &self.homeserver.public_baseurl),
            ("server_name", &self.homeserver.server_name),
            (
                "registration_shared_secret",
                &self.homeserver.registration_shared_secret,
            ),
        ] {
            combined_config.insert(key.into(), value.to_string().into());
        }
        combined_config.insert(
            "enable_registration_without_verification".into(),
            true.into(),
        );

        // Copy extra fields.
        // Note: This may include `modules` or `listeners`.
        for (key, value) in &self.homeserver.extra_fields {
            combined_config.insert(YAML::from(key.clone()), value.clone());
        }

        // Setup large default rate limits.
        let large_rate_limit: serde_yaml::Value = yaml!({
            "per_second" => 1_000_000_000,
            "burst_count" => 1_000_000_000,
        });
        for (key, rate_limit) in &[
            ("rc_message", large_rate_limit.clone()),
            ("rc_registration", large_rate_limit.clone()),
            ("rc_admin_redaction", large_rate_limit.clone()),
            (
                "rc_login",
                yaml!({
                    "address" => large_rate_limit.clone(),
                    "account" => large_rate_limit.clone(),
                    "failed_attempts" => large_rate_limit.clone(),
                }),
            ),
            (
                "rc_invites",
                yaml!({
                    "per_room" => large_rate_limit.clone(),
                    "per_user" => large_rate_limit.clone(),
                    "per_sender" => large_rate_limit,
                }),
            ),
        ] {
            if !combined_config.contains_key(key) {
                // Setup a large default rate limit.
                combined_config.insert(key.to_string().into(), rate_limit.clone());
            } else if combined_config[key].is_default() {
                // ...or the Synapse default rate limit.
                combined_config.remove(key);
            } else {
                // Otherwise, assume that the author of mx-tester.yaml knows what they're doing.
            }
        }

        // Make sure that we listen on the appropriate port.
        // For some reason, `start.py generate` tends to put port 4153 instead of HARDCODED_GUEST_PORT.
        let listeners = combined_config
            .entry(LISTENERS.into())
            .or_insert_with(|| yaml!([]));
        *listeners = yaml!([yaml!({
            "port" => if self.workers.enabled { HARDCODED_MAIN_PROCESS_HTTP_LISTENER_PORT } else { HARDCODED_GUEST_PORT },
            "tls" => false,
            "type" => "http",
            "bind_addresses" => yaml!(["::"]),
            "x_forwarded" => false,
            "resources" => yaml!([
                yaml!({
                    "names" => yaml!(["client"]),
                    "compress" => true
                }),
                yaml!({
                    "names" => yaml!(["federation"]),
                    "compress" => false
                })
            ]),
        })]);
        if self.workers.enabled {
            // Setup the replication port.
            listeners
                .as_sequence_mut()
                .unwrap() // We just set it up as a sequence
                .push(yaml!({
                    "port" => 9093,
                        "bind_address" => "127.0.0.1",
                        "type" => "http",
                        "resources" => yaml!([
                            yaml!({
                                "names" => yaml!(["replication"])
                            })
                        ])
                }));
        }

        // Copy modules config.
        let modules_root = combined_config
            .entry(MODULES.into())
            .or_insert_with(|| yaml!([]))
            .to_seq_mut()
            .ok_or_else(|| anyhow!("In homeserver.yaml, expected a sequence for key `modules`"))?;
        for module in &self.modules {
            modules_root.push(module.config.clone());
        }

        if self.workers.enabled {
            for (key, value) in std::iter::IntoIterator::into_iter([
                // No worker support without redis.
                (
                    "redis",
                    yaml!({
                        "enabled" => true,
                    }),
                ),
                // No worker support without postgresql
                (
                    "database",
                    yaml!({
                        "name" => "psycopg2",
                        "txn_limit" => 10_000,
                        "args" => yaml!({
                            "user" => "synapse",
                            "password" => "password",
                            "host" => "localhost",
                            "port" => 5432,
                            "cp_min" => 5,
                            "cp_max" => 10
                        })
                    }),
                ),
                // Deactivate a few features in the main process
                // and let a worker take over them.
                ("notify_appservices", yaml!(false)),
                ("send_federation", yaml!(false)),
                ("update_user_directory", yaml!(false)),
                ("start_pushers", yaml!(false)),
                ("url_preview_enabled", yaml!(false)),
                (
                    "url_preview_ip_range_blacklist",
                    yaml!(["255.255.255.255/32",]),
                ),
                // Also, let's get rid of that warning, it pollutes logs.
                ("suppress_key_server_warning", yaml!(true)),
            ]) {
                combined_config.insert(yaml!(key), value);
            }

            // Patch shared worker config (generated by workers_start.py) to inject modules into all workers.
            //
            // Note: In future versions, we might decide to only patch specific workers.
            let conf_path = self.synapse_workers_dir().join("shared.yaml");
            let conf_file = std::fs::File::open(&conf_path).with_context(|| {
                format!("Could not open workers shared config: {:?}", conf_path)
            })?;
            let mut config: serde_yaml::Mapping = serde_yaml::from_reader(&conf_file)
                .with_context(|| {
                    format!("Could not parse workers shared config: {:?}", conf_path)
                })?;

            let modules_root = config
                .entry(MODULES.into())
                .or_insert_with(|| yaml!([]))
                .to_seq_mut()
                .ok_or_else(|| anyhow!("In shared.yaml, expected a sequence for key `modules`"))?;
            for module in &self.modules {
                modules_root.push(module.config.clone());
            }

            for (key, value) in std::iter::IntoIterator::into_iter([
                // Disable url_preview_enabled.
                ("url_preview_enabled", yaml!(false)),
                (
                    "url_preview_ip_range_blacklist",
                    yaml!(["255.255.255.255/32"]),
                ),
                // No worker without postgres.
                (
                    "database",
                    yaml!({
                        "name" => "psycopg2",
                        "txn_limit" => 10_000,
                        "args" => yaml!({
                            "user" => "synapse",
                            "password" => "password",
                            "host" => "localhost",
                            "port" => 5432,
                            "cp_min" => 5,
                            "cp_max" => 10
                        })
                    }),
                ),
            ]) {
                config.insert(yaml!(key), value);
            }

            // Deactivate url preview
            serde_yaml::to_writer(std::fs::File::create(&conf_path)?, &combined_config)
                .context("Could not write workers shared config")?;
        }

        Ok(())
    }

    /// The directory in which we're putting all data for this test.
    ///
    /// Cleaned up upon test start.
    pub fn test_root(&self) -> PathBuf {
        self.directories.root.join(&self.name)
    }

    /// The directory in which we're putting everything related to synapse data for this test.
    pub fn synapse_root(&self) -> PathBuf {
        self.test_root().join("synapse")
    }

    /// The directory in which Synapse may write data.
    pub fn synapse_data_dir(&self) -> PathBuf {
        self.synapse_root().join("data")
    }

    /// The directory in which we're putting the configuration of workers for this test.
    pub fn synapse_workers_dir(&self) -> PathBuf {
        self.synapse_root().join("workers")
    }

    /// The directory in which we're putting files that go to subdirectories of /etc in
    /// in the guest.
    pub fn etc_dir(&self) -> PathBuf {
        self.test_root().join("etc")
    }

    /// The directory in which we publish logs.
    pub fn logs_dir(&self) -> PathBuf {
        self.test_root().join("logs")
    }

    pub fn scripts_logs_dir(&self) -> PathBuf {
        self.logs_dir().join("mx-tester")
    }

    /// A tag for the Docker image we're creating/using.
    pub fn tag(&self) -> String {
        match self.synapse {
            SynapseVersion::Docker { ref tag } => {
                format!(
                    "mx-tester-synapse-{}-{}{workers}",
                    tag,
                    self.name,
                    workers = if self.workers.enabled { "-workers" } else { "" }
                )
            }
        }
    }

    /// A name for the network we're creating/using.
    pub fn network(&self) -> String {
        format!("net-{}", self.tag())
    }

    /// The name for the container we're using to setup Synapse.
    pub fn setup_container_name(&self) -> String {
        format!(
            "mx-tester-synapse-setup-{}{}",
            self.name,
            if self.workers.enabled { "-workers" } else { "" }
        )
    }

    /// The name for the container we're using to actually run Synapse.
    pub fn run_container_name(&self) -> String {
        format!(
            "mx-tester-synapse-run-{}{}",
            self.name,
            if self.workers.enabled { "-workers" } else { "" }
        )
    }
}

/// Configurable directories for this test.
#[derive(Debug, TypedBuilder, Deserialize)]
pub struct Directories {
    /// The root of the test.
    ///
    /// All temporary files and logs are created as subdirectories of this directory.
    ///
    /// If unspecified, `mx-tester` in the platform's temporary directory.
    #[builder(default=std::env::temp_dir().join("mx-tester"))]
    pub root: PathBuf,
}
impl Default for Directories {
    fn default() -> Self {
        Directories::builder().build()
    }
}

/// The result of the test, as seen by `down()`.
pub enum Status {
    /// The test was a success.
    Success,

    /// The test was a failure.
    Failure,

    /// The test was not executed at all, we just ran `mx-tester down`.
    Manual,
}

/// The version of Synapse to use by default.
const DEFAULT_SYNAPSE_VERSION: &str = "matrixdotorg/synapse:latest";

#[derive(Debug, Deserialize)]
pub enum SynapseVersion {
    #[serde(rename = "docker")]
    Docker { tag: String },
    // FIXME: Allow using a version of Synapse that lives in a local directory
    // (this will be sufficient to also implement pulling from github develop)
}
impl Default for SynapseVersion {
    fn default() -> Self {
        Self::Docker {
            tag: DEFAULT_SYNAPSE_VERSION.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct Script {
    /// The lines of the script.
    ///
    /// Passed without change to `std::process::Command`.
    ///
    /// To communicate with the script, clients should use
    /// an exchange file.
    lines: Vec<String>,
}
impl Script {
    pub async fn run(
        &self,
        stage: &'static str,
        log_dir: &Path,
        env: &HashMap<&'static OsStr, OsString>,
    ) -> Result<(), Error> {
        debug!("Running with environment variables {:#?}", env);
        println!(
            "** running {} script. See stdout and stderr captures in {:?}",
            stage,
            log_dir.join(stage)
        );
        let _ = std::fs::remove_dir(log_dir.join(stage).as_path().with_extension("log"));
        let _ = std::fs::remove_dir(log_dir.join(stage).as_path().with_extension("out"));
        let executor = Executor::try_new().context("Cannot instantiate executor")?;
        for line in &self.lines {
            println!("*** {}", line);
            let mut command = executor
                .command(line)
                .with_context(|| format!("Could not interpret `{}` as shell script", line))?;
            command.envs(env);
            debug!("Running command {:?}", command);
            command
                .spawn_logged(log_dir, stage, line)
                .await
                .with_context(|| format!("Error within line {line}", line = line))?;
        }
        println!("** running {} script success", stage);
        Ok(())
    }
}

/// A script for `build`.
#[derive(Debug, Deserialize)]
pub struct ModuleConfig {
    /// The name of the module.
    ///
    /// This name is used to create a subdirectory.
    name: String,

    /// A script to build and copy the module in the directory
    /// specified by environment variable `MX_TEST_MODULE_DIR`.
    ///
    /// This script will be executed in the **host**.
    build: Script,

    /// A script to install dependencies.
    ///
    /// This script will be executed in the **guest**.
    #[serde(default)]
    install: Option<Script>,

    /// Additional environment information to use in the **guest**.
    #[serde(default)]
    env: HashMap<String, String>,

    /// Additional resources to copy from the **host** into the **guest**.
    /// Key: Guest path, relative to the module's directory.
    /// Value: Guest path, relative to the project directory.
    #[serde(default)]
    copy: HashMap<String, String>,

    /// A Yaml config to copy into homeserver.yaml.
    /// See https://matrix-org.github.io/synapse/latest/modules/index.html
    ///
    /// This typically looks like
    /// ```yaml
    /// module: python_module_name
    /// config:
    ///   key: value
    /// ```
    config: serde_yaml::Value,
}

/// A script for `up`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum UpScript {
    /// If `up` and/or `down` are specified, take them into account.
    FullUpScript(FullUpScript),

    /// Otherwise, it's a simple script.
    SimpleScript(Script),
}
impl Default for UpScript {
    fn default() -> Self {
        UpScript::FullUpScript(FullUpScript::default())
    }
}

/// A script for `up`.
#[derive(Debug, Deserialize, Default)]
pub struct FullUpScript {
    /// Code to run before bringing up the image.
    before: Option<Script>,

    /// Code to run after bringing up the image.
    after: Option<Script>,
}

/// A script for `down`.
#[derive(Debug, Deserialize)]
pub struct DownScript {
    /// Code to run in case the test is a success.
    success: Option<Script>,

    /// Code to run in case the test is a failure.
    failure: Option<Script>,

    /// Code to run regardless of the result of the test.
    ///
    /// Executed after `success` or `failure`.
    finally: Option<Script>,
}

/// Start a Synapse container.
///
/// - `cmd`: a shell command to execute;
/// - `detach`: if `true`, the Synapse container will continue executing past the end of this function and process;
async fn start_synapse_container(
    docker: &Docker,
    config: &Config,
    container_name: &str,
    cmd: Vec<String>,
    detach: bool,
) -> Result<(), Error> {
    let data_dir = config.synapse_data_dir();
    let data_dir = data_dir.as_path();

    let mut env = vec![
        format!("SYNAPSE_SERVER_NAME={}", config.homeserver.server_name),
        "SYNAPSE_REPORT_STATS=no".into(),
        "SYNAPSE_CONFIG_DIR=/data".into(),
        format!(
            "SYNAPSE_HTTP_PORT={}",
            if config.workers.enabled {
                HARDCODED_MAIN_PROCESS_HTTP_LISTENER_PORT
            } else {
                HARDCODED_GUEST_PORT
            }
        ),
    ];
    if config.workers.enabled {
        // The list of workers to launch, as copied from Complement.
        // It has two instances of `event_persister` by design, in order
        // to launch two event persisters.
        env.push("SYNAPSE_WORKER_TYPES=event_persister, event_persister, background_worker, frontend_proxy, event_creator, user_dir, media_repository, federation_inbound, federation_reader, federation_sender, synchrotron, appservice, pusher".to_string());
        env.push("SYNAPSE_WORKERS_WRITE_LOGS_TO_DISK=1".to_string());
    }
    let env = env;
    debug!("We need to create container for {}", container_name);

    // Generate configuration to open and map ports.
    let mut host_port_bindings = HashMap::new();
    let mut exposed_ports = HashMap::new();
    for mapping in config.docker.port_mapping.iter().chain(
        [PortMapping {
            host: config.homeserver.host_port,
            guest: HARDCODED_GUEST_PORT,
        }]
        .iter(),
    ) {
        let key = format!("{}/tcp", mapping.guest);
        host_port_bindings.insert(
            key.clone(),
            Some(vec![PortBinding {
                host_port: Some(format!("{}", mapping.host)),
                ..PortBinding::default()
            }]),
        );
        exposed_ports.insert(key.clone(), HashMap::new());
    }
    debug!("port_bindings: {:#?}", host_port_bindings);

    debug!("Creating container {}", container_name);
    let response = docker
        .create_container(
            Some(CreateContainerOptions {
                name: container_name,
            }),
            BollardContainerConfig {
                env: Some(env.clone()),
                exposed_ports: Some(exposed_ports),
                hostname: Some(config.docker.hostname.clone()),
                host_config: Some(HostConfig {
                    log_config: Some(HostConfigLogConfig {
                        typ: Some("json-file".to_string()),
                        config: None,
                    }),
                    // Synapse has a tendency to not start correctly
                    // or to stop shortly after startup. The following
                    // restart policy seems to help a lot.
                    restart_policy: Some(RestartPolicy {
                        name: Some(RestartPolicyNameEnum::ON_FAILURE),
                        maximum_retry_count: Some(MAX_SYNAPSE_RESTART_COUNT),
                    }),
                    // Extremely large memory allowance.
                    memory_reservation: Some(MEMORY_ALLOCATION_BYTES),
                    memory_swap: Some(-1),
                    // Mount guest directories as host directories.
                    binds: Some(vec![
                        // Synapse logs, etc.
                        format!("{}:/data:rw", data_dir.as_os_str().to_string_lossy()),
                        // Everything below this point is for workers.
                        format!(
                            "{}:/conf/workers:rw",
                            config.synapse_workers_dir().to_string_lossy()
                        ),
                        format!(
                            "{}:/etc/nginx/conf.d:rw",
                            config.etc_dir().join("nginx").to_string_lossy()
                        ),
                        format!(
                            "{}:/etc/supervisor/conf.d:rw",
                            config.etc_dir().join("supervisor").to_string_lossy()
                        ),
                        format!(
                            "{}:/var/log/nginx:rw",
                            config.logs_dir().join("nginx").to_string_lossy()
                        ),
                        format!(
                            "{}:/var/log/workers:rw",
                            config.logs_dir().join("workers").to_string_lossy()
                        ),
                    ]),
                    // Expose guest port `guest_mapping` as `host_mapping`.
                    port_bindings: Some(host_port_bindings),
                    // Enable access to host as `host.docker.internal` from the guest.
                    // On macOS and Windows, this is expected to be transparent but
                    // on Linux, an option needs to be added.
                    #[cfg(target_os = "linux")]
                    extra_hosts: Some(vec!["host.docker.internal:host-gateway".to_string()]),
                    ..HostConfig::default()
                }),
                image: Some(config.tag()),
                attach_stderr: Some(true),
                attach_stdout: Some(true),
                attach_stdin: Some(false),
                cmd: Some(cmd.clone()),
                // Specify that a few directories may be mounted.
                // The empty hashmap... is an oddity of the Docker Engine API.
                volumes: Some(
                    vec![
                        ("/data".to_string(), HashMap::new()),
                        ("/conf/workers".to_string(), HashMap::new()),
                        ("/etc/nginx/conf.d".to_string(), HashMap::new()),
                        ("/etc/supervisor/conf.d".to_string(), HashMap::new()),
                        ("/var/log/workers".to_string(), HashMap::new()),
                    ]
                    .into_iter()
                    .collect(),
                ),
                tty: Some(false),
                #[cfg(unix)]
                user: Some(format!("{}", nix::unistd::getuid())),
                ..BollardContainerConfig::default()
            },
        )
        .await
        .context("Failed to build container")?;

    // For debugging purposes, try and find out when/why the container stops.
    let mut wait = docker.wait_container(
        container_name,
        Some(WaitContainerOptions {
            condition: "not-running",
        }),
    );
    {
        let container_name = container_name.to_string();
        tokio::task::spawn(async move {
            debug!(target: "mx-tester-wait", "{} Container started", container_name);
            while let Some(next) = wait.next().await {
                let response = next.context("Error while waiting for container to stop")?;
                debug!(target: "mx-tester-wait", "{} {:#?}", container_name, response);
            }
            debug!(target: "mx-tester-wait", "{} Container is now down", container_name);
            Ok::<(), Error>(())
        });
    }

    for warning in response.warnings {
        warn!(target: "creating-container", "{}", warning);
    }

    // ... add the container to the network.
    docker
        .connect_network(
            config.network().as_ref(),
            ConnectNetworkOptions {
                container: container_name,
                endpoint_config: EndpointSettings::default(),
            },
        )
        .await
        .context("Failed to connect container")?;

    let is_container_running = docker.is_container_running(container_name).await?;
    if !is_container_running {
        docker
            .start_container(container_name, None::<StartContainerOptions<String>>)
            .await
            .context("Failed to start container")?;
        let mut logs = docker.logs(
            container_name,
            Some(LogsOptions {
                follow: true,
                stdout: true,
                stderr: true,
                tail: "10",
                ..LogsOptions::default()
            }),
        );

        // Write logs to the synapse data directory.
        let log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.logs_dir().join("docker").join(format!(
                "{}.log",
                if detach { "up-run-down" } else { "build" }
            )))
            .await?;
        let mut buffer = BufWriter::new(log_file);
        tokio::task::spawn(async move {
            debug!(target: "mx-tester-log", "Starting log watcher");
            while let Some(next) = logs.next().await {
                match next {
                    Ok(content) => {
                        debug!(target: "mx-tester-log", "{}", content);
                        buffer.write_all(format!("{}", content).as_bytes()).await?;
                        buffer.flush().await?;
                    }
                    Err(err) => {
                        error!(target: "mx-tester-log", "{}", err);
                        buffer
                            .write_all(format!("ERROR: {}", err).as_bytes())
                            .await?;
                        buffer.flush().await?;
                        return Err(err).context("Error in log");
                    }
                }
            }
            debug!(target: "mx-tester-log", "Stopped log watcher");
            Ok(())
        });
    }

    let cleanup = if config.autoclean_on_error {
        Some(Cleanup::new(config))
    } else {
        None
    };
    let exec = docker
        .create_exec(
            container_name,
            CreateExecOptions::<Cow<'_, str>> {
                cmd: Some(cmd.into_iter().map(|s| s.into()).collect()),
                env: Some(env.into_iter().map(|s| s.into()).collect()),
                #[cfg(unix)]
                user: Some(format!("{}", nix::unistd::getuid()).into()),
                ..CreateExecOptions::default()
            },
        )
        .await
        .context("Error while preparing to Synapse container")?;
    let execution = docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                detach,
                ..StartExecOptions::default()
            }),
        )
        .await
        .context("Error starting Synapse container")?;

    if !detach {
        let log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.logs_dir().join("docker").join(format!(
                "{}.out",
                if detach { "up-run-down" } else { "build" }
            )))
            .await?;
        let mut buffer = BufWriter::new(log_file);
        tokio::task::spawn(async move {
            debug!(target: "synapse", "Launching Synapse container");
            match execution {
                bollard::exec::StartExecResults::Attached {
                    mut output,
                    input: _,
                } => {
                    while let Some(data) = output.next().await {
                        let output = data.context("Error during run")?;
                        debug!(target: "synapse", "{}", output);
                        buffer.write_all(format!("{}", output).as_bytes()).await?;
                        buffer.flush().await?;
                    }
                }
                bollard::exec::StartExecResults::Detached => panic!(),
            }
            debug!(target: "synapse", "Synapse container finished");
            Ok::<(), Error>(())
        })
        .await??;
    }
    cleanup.disarm();
    Ok(())
}

/// Rebuild the Synapse image with modules.
pub async fn build(docker: &Docker, config: &Config) -> Result<(), Error> {
    // This will break (on purpose) once we extend `SynapseVersion`.
    let SynapseVersion::Docker {
        tag: ref docker_tag,
    } = config.synapse;
    let setup_container_name = config.setup_container_name();
    let run_container_name = config.run_container_name();

    println!("\n* build step: starting");

    // Remove any trace of a previous build. Ignore failures.
    let _ = docker.stop_container(&run_container_name, None).await;
    let _ = docker.remove_container(&run_container_name, None).await;
    let _ = docker.stop_container(&setup_container_name, None).await;
    let _ = docker.remove_container(&setup_container_name, None).await;
    let _ = docker.remove_image(config.tag().as_ref(), None, None).await;

    let synapse_root = config.synapse_root();
    let _ = std::fs::remove_dir_all(config.test_root());
    let modules_log_dir = config.scripts_logs_dir().join("modules");
    for dir in &[
        &config.synapse_data_dir(),
        &config.synapse_workers_dir(),
        &config.etc_dir().join("nginx"),
        &config.etc_dir().join("supervisor"),
        &config.logs_dir().join("docker"),
        &config.logs_dir().join("nginx"),
        &config.logs_dir().join("workers"),
        &modules_log_dir,
    ] {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Could not create directory {:#?}", dir,))?;
    }

    // Build modules
    println!("** building modules");
    let mut env = config.shared_env_variables()?;

    for module in &config.modules {
        let path = synapse_root.join(&module.name);
        env.insert(&*MX_TEST_MODULE_DIR, path.as_os_str().into());
        debug!(
            "Calling build script for module {} with MX_TEST_DIR={:#?}",
            &module.name, path
        );
        let log_dir = modules_log_dir.join(&module.name);
        std::fs::create_dir_all(&log_dir)
            .with_context(|| format!("Could not create directory {:#?}", log_dir,))?;
        module
            .build
            .run("build", &log_dir, &env)
            .await
            .context("Error running build script")?;
        debug!("Completed one module.");
    }
    println!("** building modules success");

    // Prepare resource files.
    if config.workers.enabled {
        let conf_dir = synapse_root.join("conf");
        std::fs::create_dir_all(&conf_dir)
            .context("Could not create directory for worker configuration file")?;
        let data = [
            // These files are used by `workers_start.py` to generate worker configuration.
            // They have been copied manually from Synapse's git repo.
            // Hopefully, in the future, Synapse+worker images will be available on DockerHub.
            (
                conf_dir.join("worker.yaml.j2"),
                include_str!("../res/workers/worker.yaml.j2"),
            ),
            (
                conf_dir.join("shared.yaml.j2"),
                include_str!("../res/workers/shared.yaml.j2"),
            ),
            (
                conf_dir.join("supervisord.conf.j2"),
                include_str!("../res/workers/supervisord.conf.j2"),
            ),
            (
                conf_dir.join("nginx.conf.j2"),
                include_str!("../res/workers/nginx.conf.j2"),
            ),
            (
                conf_dir.join("log.config"),
                include_str!("../res/workers/log.config"),
            ),
            // workers_start.py is adapted from Synapse's git repo.
            (
                synapse_root.join("workers_start.py"),
                include_str!("../res/workers/workers_start.py"),
            ),
            // setup postgres user and database
            (
                conf_dir.join("postgres.sql"),
                include_str!("../res/workers/postgres.sql"),
            ),
        ];
        for (path, content) in &data {
            std::fs::write(path, content).with_context(|| {
                format!("Could not inject worker configuration file {:?}", path)
            })?;
        }
    }

    // Prepare Dockerfile including modules.
    let dockerfile_content = format!("
# A custom Dockerfile to rebuild synapse from the official release + plugins

FROM {docker_tag}

VOLUME [\"/data\", \"/conf/workers\", \"/etc/nginx/conf.d\", \"/etc/supervisor/conf.d\", \"/var/log/workers\"]

# We're not running as root, to avoid messing up with the host
# filesystem, so we need a proper user. We give it the current
# use's uid to make sure that files written by this Docker image
# can be read and removed by the host's user.
# Note that we need tty to workaround the following Docker issue:
# https://github.com/moby/moby/issues/31243#issuecomment-406825071
RUN useradd mx-tester {maybe_uid} --groups sudo,tty

# Add a password, to be able to run sudo. We'll use it to
# chmod files.
RUN echo \"mx-tester:password\" | chpasswd

# Show the Synapse version, to aid with debugging.
RUN pip show matrix-synapse

{maybe_setup_workers}

# Copy and install custom modules.
RUN mkdir /mx-tester
{setup}
{env}
{copy_modules}
{copy_resources}
{install}

ENTRYPOINT []

EXPOSE {synapse_http_port}/tcp 8009/tcp 8448/tcp
",
    docker_tag = docker_tag,
    // Module setup steps, as per `config.modules[_].install`.
    setup = config.modules.iter()
        .filter_map(|module| module.install.as_ref().map(|script| format!("## Setup {}\n{}\n", module.name, script.lines.iter().map(|line| format!("RUN {}", line)).format("\n"))))
        .format("\n"),
    // Module env changes, as per `config.modules[_].env`.
    env = config.modules.iter()
        .map(|module| module.env.iter()
            .map(|(key, value)| format!("ENV {}={}\n", key, value))
            .format("")
        ).format(""),
    copy_modules = config.modules.iter()
        // FIXME: We probably want to test what happens with weird characters. Perhaps we'll need to somehow escape module.
        .map(|module| format!("COPY {module} /mx-tester/{module}", module=module.name))
        .format("\n"),
    // Modules additional resources, as per `config.modules[_].copy`.
    copy_resources = config.modules.iter()
        .map(|module| module.copy.iter()
            .map(move |(dest, source)| format!("COPY {source} /mx-tester/{module}/{dest}\n",
                dest = dest,
                source = source,
                module = module.name,
            ))
            .format("")
        ).format(""),
    // Modules copy and `pip` install.
    install = config.modules.iter()
        // FIXME: We probably want to test what happens with weird characters. Perhaps we'll need to somehow escape module.
        .map(|module| format!("RUN /usr/local/bin/python -m pip install /mx-tester/{module}", module=module.name))
        .format("\n"),
    // Configure user id.
    maybe_uid = {
        let my_uid = nix::unistd::getuid();
        if my_uid != nix::unistd::ROOT {
           // We're running mx-tester as a regular user.
            // Docker/Synapse should be executed as the same user, otherwise we won't be
            // able to look at or remove all the files created by them.
            Cow::from(format!("--uid {}", my_uid))
        } else {
            // We're running mx-tester as root.
            // This can happen in CI, for instance.
            // In this case, let `useradd` pick a uid for our user. We can't use
            // the root uid as it's already taken by root in the guest environment
            // and any uid will do since we're root in the host environment anyway,
            // so we can look at and remove any file created by other users.
            Cow::from("")
        }
    },
    synapse_http_port = HARDCODED_GUEST_PORT,
    maybe_setup_workers =
    if config.workers.enabled {
"
# Install dependencies
RUN apt-get update && apt-get install -y postgresql postgresql-client-13 supervisor redis nginx sudo lsof

# For workers, we're not using start.py but workers_start.py
# (which does call start.py, but that's a long story).
COPY workers_start.py /workers_start.py
COPY conf/* /conf/

# We're not going to be running workers_start.py as root, so
# let's make sure that it *can* run, write to /etc/nginx & co.
RUN chmod ugo+rx /workers_start.py && chown mx-tester /workers_start.py
"
    } else {
        ""
    }
    );
    debug!("dockerfile {}", dockerfile_content);

    let dockerfile_path = synapse_root.join("Dockerfile");
    std::fs::write(&dockerfile_path, dockerfile_content)
        .with_context(|| format!("Could not write file {:#?}", dockerfile_path,))?;

    debug!("Building tar file");
    let docker_dir_path = config.test_root().join("tar");
    std::fs::create_dir_all(&docker_dir_path)
        .with_context(|| format!("Could not create directory {:#?}", docker_dir_path,))?;
    let body = {
        // Build the tar file.
        let tar_path = docker_dir_path.join("docker.tar");
        {
            let tar_file = std::fs::File::create(&tar_path)?;
            let mut tar_builder = tar::Builder::new(std::io::BufWriter::new(tar_file));
            debug!("tar: adding directory {:#?}", synapse_root);
            tar_builder
                .append_dir_all("", &synapse_root)
                .with_context(|| format!("Error while creating tar for {:#?}", &synapse_root))?;
            tar_builder
                .finish()
                .with_context(|| format!("Error finalizing tar for {:#?}", &synapse_root))?
        }

        let tar_file = tokio::fs::File::open(&tar_path).await?;
        let stream = FramedRead::new(tar_file, BytesCodec::new());
        hyper::Body::wrap_stream(stream)
    };
    let logs_path = config.logs_dir().join("docker").join("build.log");
    println!(
        "** building Docker image. Logs will be stored at {:?}",
        logs_path
    );
    debug!("Building image with tag {}", config.tag());
    {
        let mut log =
            std::fs::File::create(logs_path).context("Could not create docker build logs")?;
        let mut stream = docker.build_image(
            bollard::image::BuildImageOptions {
                pull: true,
                nocache: true,
                t: config.tag(),
                q: false,
                rm: true,
                ..Default::default()
            },
            config.credentials.serveraddress.as_ref().map(|server| {
                let mut credentials = HashMap::new();
                credentials.insert(server.clone(), config.credentials.clone());
                credentials
            }),
            Some(body),
        );
        while let Some(result) = stream.next().await {
            let info = result.context("Daemon `docker build` indicated an error")?;
            if let Some(ref error) = info.error {
                return Err(anyhow!("Error while building an image: {}", error,));
            }
            if let Some(ref progress) = info.progress {
                debug!("Build image progress {:#?}", info);
                log.write_all(progress.as_bytes())
                    .context("Could not write docker build logs")?;
            }
        }
    }
    debug!("Image built");
    println!("** building Docker image success");

    println!("* build step: success");
    Ok(())
}

/// Bring things up. Returns any environment variables to pass to the run script.
pub async fn up(docker: &Docker, config: &Config) -> Result<(), Error> {
    // This will break (on purpose) once we extend `SynapseVersion`.
    let SynapseVersion::Docker { .. } = config.synapse;
    let cleanup = if config.autoclean_on_error {
        Some(Cleanup::new(config))
    } else {
        None
    };

    println!("\n* up step: starting");
    // Create the network if necessary.
    // We'll add the container once it's available.
    let network_name = config.network();
    debug!("We'll need network {}", network_name);
    if !docker.is_network_up(&network_name).await? {
        debug!("Creating network {}", network_name);
        docker
            .create_network(CreateNetworkOptions {
                name: Cow::from(network_name.as_str()),
                check_duplicate: true,
                attachable: true,
                ..CreateNetworkOptions::default()
            })
            .await?;
        assert!(
            docker.is_network_up(&network_name).await?,
            "The network should now be up"
        );
        debug!("Network is now up");
    } else {
        // This can happen for instance if a script needs to
        // spawn another image on the same network and creates
        // that network manually.
        debug!("Network {} already exists", network_name);
    }

    // Only execute the `up` script once the network is up,
    // in case we want to e.g. bring up images that need
    // that same network.
    let script_log_dir = config.scripts_logs_dir();
    match config.up {
        Some(UpScript::FullUpScript(FullUpScript {
            before: Some(ref script),
            ..
        }))
        | Some(UpScript::SimpleScript(ref script)) => {
            let env = config.shared_env_variables()?;
            script
                .run("up", &script_log_dir, &env)
                .await
                .context("Error running `up` script (before)")?;
        }
        _ => {}
    }

    let setup_container_name = config.setup_container_name();
    let run_container_name = config.run_container_name();

    // Create the synapse data directory.
    // We'll use it as volume.
    let synapse_data_directory = config.synapse_data_dir();
    std::fs::create_dir_all(&synapse_data_directory)
        .with_context(|| format!("Cannot create directory {:#?}", synapse_data_directory))?;

    // Cleanup leftovers.
    let homeserver_path = synapse_data_directory.join("homeserver.yaml");
    let _ = std::fs::remove_file(&homeserver_path);

    // Start a container to generate homeserver.yaml.
    start_synapse_container(
        docker,
        config,
        &setup_container_name,
        if config.workers.enabled {
            vec!["/workers_start.py".to_string(), "generate".to_string()]
        } else {
            vec!["/start.py".to_string(), "generate".to_string()]
        },
        false,
    )
    .await
    .context("Couldn't generate homeserver.yaml")?;

    // HACK: I haven't found a way to reuse the container with a different cmd
    // (the API looks like it supports overriding cmds when creating an
    // Exec but doesn't seem to actually implement this feature), so
    // we stop and remove the container, we'll create a new one when
    // we're ready to start Synapse.
    debug!("done generating");
    let _ = docker.stop_container(&setup_container_name, None).await;
    let _ = docker.remove_container(&setup_container_name, None).await;
    docker.wait_container_removed(&setup_container_name).await?;

    debug!("Updating homeserver.yaml");
    // Apply config from mx-tester.yml to the homeserver.yaml that was just created
    config
        .patch_homeserver_config()
        .context("Error updating homeserver config")?;

    // Docker has a tendency to return before containers are fully torn down.
    // Let's make extra-sure by waiting until the container is not running
    // anymore *and* the ports are free.
    while docker.is_container_running(&setup_container_name).await? {
        debug!(
            "Waiting until docker container {} is down before relaunching it",
            setup_container_name
        );
        tokio::time::sleep(std::time::Duration::new(5, 0)).await;
    }

    println!(
        "** starting Synapse. Logs will be stored at {:?}",
        config.logs_dir().join("docker").join("up-run-down.log")
    );
    start_synapse_container(
        docker,
        config,
        &run_container_name,
        if config.workers.enabled {
            vec!["/workers_start.py".to_string(), "start".to_string()]
        } else {
            vec!["/start.py".to_string()]
        },
        true,
    )
    .await
    .context("Failed to start Synapse")?;

    debug!("Synapse should now be launched and ready");

    // We should now be able to register users.
    //
    // As of this writing, we're not sure whether the `synapse_is_responsive` manipulation
    // above works. If it doesn't, we can still have a case in which Synapse won't start,
    // causing `handle_user_registration` to loop endlessly. The `timeout` should make
    // sure that we fail properly and with an understandable error message.
    let registration = async {
        handle_user_registration(config)
            .await
            .context("Failed to setup users")
    };

    if config.workers.enabled {
        // With workers, registration is so long that we don't want to timeou.
        registration.await?;
    } else {
        match tokio::time::timeout(TIMEOUT_USER_REGISTRATION_SIMPLE, registration).await {
            Err(_) => {
                // Timeout.
                panic!(
                    "User registration is taking too long. {is_running}",
                    is_running = if docker.is_container_running(&run_container_name).await? {
                        "Container is running, so this is usually an error in Synapse or modules."
                    } else {
                        "For some reason, the Docker image has stopped."
                    },
                );
            }
            Ok(result) => result,
        }?
    };
    if let Some(UpScript::FullUpScript(FullUpScript {
        after: Some(ref script),
        ..
    })) = config.up
    {
        let env = config.shared_env_variables()?;
        script
            .run("up", &script_log_dir, &env)
            .await
            .context("Error running `up` script (after)")?;
    }

    cleanup.disarm();

    println!("* up step: success");
    Ok(())
}

/// Bring things down.
pub async fn down(docker: &Docker, config: &Config, status: Status) -> Result<(), Error> {
    // This will break (on purpose) once we extend `SynapseVersion`.
    let SynapseVersion::Docker { .. } = config.synapse;
    let run_container_name = config.run_container_name();

    println!("\n* down step: starting");

    // Store results, we'll report them after we've brought down everything
    // that we can bring down.
    let script_log_dir = config.scripts_logs_dir();
    let script_result = if let Some(ref down_script) = config.down {
        let env = config.shared_env_variables()?;
        // First run on_failure/on_success.
        // Store errors for later.
        let result = match (status, down_script) {
            (
                Status::Failure,
                DownScript {
                    failure: Some(ref on_failure),
                    ..
                },
            ) => on_failure
                .run("on_failure", &script_log_dir, &env)
                .await
                .context("Error while running script `down/failure`"),
            (
                Status::Success,
                DownScript {
                    success: Some(ref on_success),
                    ..
                },
            ) => on_success
                .run("on_success", &script_log_dir, &env)
                .await
                .context("Error while running script `down/success`"),
            _ => Ok(()),
        };
        // Then run on_always.
        if let Some(ref on_always) = down_script.finally {
            result.and(
                on_always
                    .run("on_always", &script_log_dir, &env)
                    .await
                    .context("Error while running script `down/finally`"),
            )
        } else {
            result
        }
    } else {
        Ok(())
    };

    debug!(target: "mx-tester-down", "Taking down synapse.");
    let stop_container_result = match docker.stop_container(&run_container_name, None).await {
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if (200..300).contains(&status_code) => {
            debug!(target: "mx-tester-down", "Synapse container stopped: {}", message);
            Ok(())
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if status_code == 304 => {
            debug!(target: "mx-tester-down", "Synapse container was already down: {}", message);
            Ok(())
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if status_code == 404 => {
            debug!(target: "mx-tester-down", "Synapse container not found for stopping: {}", message);
            Ok(())
        }
        Err(err) => Err(err).context("Error stopping container"),
        Ok(_) => {
            debug!(target: "mx-tester-down", "Synapse container stopped");
            Ok(())
        }
    };

    let remove_container_result = match docker.remove_container(&run_container_name, None).await {
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if (200..300).contains(&status_code) => {
            debug!(target: "mx-tester-down", "Synapse container removed: {}", message);
            Ok(())
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if status_code == 304 => {
            debug!(target: "mx-tester-down", "Synapse container was already removed: {}", message);
            Ok(())
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if status_code == 404 => {
            debug!(target: "mx-tester-down", "Synapse container not found for removing: {}", message);
            Ok(())
        }
        Err(err) => Err(err).context("Error removing container"),
        Ok(_) => {
            debug!(target: "mx-tester-down", "Synapse container removed");
            Ok(())
        }
    };

    debug!(target: "mx-tester-down", "Taking down network.");
    let remove_network_result = match docker.remove_network(config.network().as_ref()).await {
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if (200..300).contains(&status_code) => {
            debug!(target: "mx-tester-down", "Network removed: {}", message);
            Ok(())
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if status_code == 304 => {
            debug!(target: "mx-tester-down", "Network was already removed: {}", message);
            Ok(())
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            message,
            status_code,
        }) if status_code == 404 => {
            debug!(target: "mx-tester-down", "Network not found for removing: {}", message);
            Ok(())
        }
        Err(err) => Err(err).context("Error removing network"),
        Ok(_) => {
            debug!(target: "mx-tester-down", "Network removed");
            Ok(())
        }
    };

    println!("* down step: complete");
    // Finally, report any problem.
    script_result
        .and(stop_container_result)
        .and(remove_container_result)
        .and(remove_network_result)
}

/// Run the testing script.
pub async fn run(_docker: &Docker, config: &Config) -> Result<(), Error> {
    println!("\n* run step: starting");
    if let Some(ref code) = config.run {
        let env = config.shared_env_variables()?;
        code.run("run", &config.scripts_logs_dir(), &env)
            .await
            .context("Error running `run` script")?;
    }
    println!("* run step: success");
    Ok(())
}

/// Utility methods for `Docker`.
#[async_trait::async_trait]
trait DockerExt {
    /// Check whether a network exists.
    async fn is_network_up(&self, name: &str) -> Result<bool, Error>;

    /// Check whether a container is currently running.
    async fn is_container_running(&self, name: &str) -> Result<bool, Error>;

    /// Check whether a container has been created (running or otherwise).
    async fn is_container_created(&self, name: &str) -> Result<bool, Error>;

    async fn wait_container_removed(&self, name: &str) -> Result<(), Error>;
}

#[async_trait::async_trait]
impl DockerExt for Docker {
    /// Check whether a network exists.
    async fn is_network_up(&self, name: &str) -> Result<bool, Error> {
        let networks = self
            .list_networks(Some(ListNetworksOptions {
                filters: vec![("name", vec![name])].into_iter().collect(),
            }))
            .await?;
        // `filters` actually filter by substring, so we need to double-check the result.
        debug!("is_network_up {:#?}", networks);
        Ok(networks
            .into_iter()
            .filter_map(|network| network.name)
            .any(|candidate_name| candidate_name.as_str() == name))
    }

    /// Check whether a container is currently running.
    async fn is_container_running(&self, name: &str) -> Result<bool, Error> {
        let containers = self
            .list_containers(Some(ListContainersOptions {
                // Check for running containers only
                all: false,
                filters: vec![("name", vec![name])].into_iter().collect(),
                ..ListContainersOptions::default()
            }))
            .await?;
        // `filters` actually filter by substring, so we need to double-check the result.
        debug!("is_container_running {:#?}", containers);
        let found = containers
            .into_iter()
            .flat_map(|container| container.names)
            .flat_map(|names| names.into_iter())
            .any(|container_name| container_name.as_str() == name);
        Ok(found)
    }

    /// Check whether a container has been created (running or otherwise).
    async fn is_container_created(&self, name: &str) -> Result<bool, Error> {
        let containers: Vec<_> = self
            .list_containers(Some(ListContainersOptions {
                // Check for both running and non-running containers.
                all: true,
                // FIXME: This filter seems to filter by substring. That's... not reliable.
                filters: vec![("name", vec![name])].into_iter().collect(),
                ..ListContainersOptions::default()
            }))
            .await?;
        // `filters` actually filter by substring, so we need to double-check the result.
        debug!("is_container_created {:#?}", containers);
        let found = containers
            .into_iter()
            .flat_map(|container| container.names)
            .flat_map(|names| names.into_iter())
            .any(|container_name| container_name.as_str() == name);
        Ok(found)
    }

    async fn wait_container_removed(&self, name: &str) -> Result<(), Error> {
        let mut stream = self.wait_container(
            name,
            Some(WaitContainerOptions {
                condition: "removed",
            }),
        );
        'waiting: while let Some(result) = stream.next().await {
            match result {
                Ok(bollard::models::ContainerWaitResponse {
                    error: Some(error), ..
                }) => {
                    return Err(anyhow!(
                        "Error while waiting for container {} to be removed: {:?}",
                        name,
                        error
                    ));
                }
                Ok(_) => {
                    // In progress.
                }
                Err(bollard::errors::Error::DockerResponseServerError { status_code, .. })
                    if status_code == 304 =>
                {
                    // We started waiting too late, the container is already removed.
                    break 'waiting;
                }
                Err(bollard::errors::Error::DockerResponseServerError { status_code, .. })
                    if status_code == 404 =>
                {
                    // We started waiting too late, the container is already removed.
                    break 'waiting;
                }
                Err(err) => {
                    return Err(err)
                        .context(format!("Waiting for container {} to be removed", name));
                }
            }
        }
        Ok(())
    }
}

/// Utility trait: determine whether a yaml value is a stand-in for "please use the default"
/// value provided by Synapse.
trait IsDefault {
    fn is_default(&self) -> bool;
}
impl IsDefault for serde_yaml::Value {
    fn is_default(&self) -> bool {
        if let Some(str) = self.as_str() {
            if str == "synapse-default" {
                return true;
            }
        }
        false
    }
}
