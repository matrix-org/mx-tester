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

pub mod registration;

use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::{OsStr, OsString},
    io::{LineWriter, Write},
    ops::Not,
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
use futures_util::stream::StreamExt;
use itertools::Itertools;
use lazy_static::lazy_static;
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::codec::{BytesCodec, FramedRead};
use typed_builder::TypedBuilder;

use registration::{handle_user_registration, User};

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
}

/// The amount of memory to allocate
const MEMORY_ALLOCATION_BYTES: i64 = 4 * 1024 * 1024 * 1024;

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
    #[serde(default = "DockerConfig::default_port_mapping")]
    #[builder(default = DockerConfig::default_port_mapping())]
    pub port_mapping: Vec<PortMapping>,
}

impl Default for DockerConfig {
    fn default() -> DockerConfig {
        DockerConfig {
            hostname: Self::default_hostname(),
            port_mapping: Self::default_port_mapping(),
        }
    }
}

impl DockerConfig {
    fn default_hostname() -> String {
        "synapse".to_string()
    }
    fn default_port_mapping() -> Vec<PortMapping> {
        vec![PortMapping {
            host: 9999,
            guest: 8008,
        }]
    }
}

/// Configuration for the homeserver.
///
/// This will be applied to homeserver.yaml.
#[derive(Debug, Deserialize, Serialize, TypedBuilder)]
pub struct HomeserverConfig {
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

/// The contents of a mx-tester.yaml
#[derive(Debug, TypedBuilder, Deserialize)]
pub struct Config {
    /// A name for this test.
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
}

impl Config {
    /// Create a map containing the environment variables that are common
    /// to all scripts.
    ///
    /// Callers may add additional variables that are specific to a given
    /// script step.
    pub fn shared_env_variables(&self) -> Result<HashMap<&'static OsStr, OsString>, Error> {
        let synapse_root = self.synapse_root();
        let script_tmpdir = self.directories.root.join("scripts");
        std::fs::create_dir_all(&script_tmpdir)
            .with_context(|| format!("Could not create directory {:#?}", script_tmpdir,))?;
        let curdir = std::env::current_dir()?;
        let mut env: HashMap<&'static OsStr, _> = HashMap::new();
        env.insert(&*MX_TEST_SYNAPSE_DIR, synapse_root.as_os_str().into());
        env.insert(&*MX_TEST_SCRIPT_TMPDIR, script_tmpdir.as_os_str().into());
        env.insert(&*MX_TEST_CWD, curdir.as_os_str().into());
        Ok(env)
    }

    /// Patch the homeserver.yaml at the given path (usually one that has been generated by synapse)
    /// with the properties in this struct (which will usually have been provided from mx-tester.yaml)
    pub fn patch_homeserver_config(&self) -> Result<(), Error> {
        use serde_yaml::{Mapping, Value as YAML};
        let target_path = self.synapse_root().join("data").join("homeserver.yaml");
        debug!("Attempting to open {:#?}", target_path);
        let config_file = std::fs::File::open(&target_path)
            .context("Could not open the homeserver.yaml generated by synapse")?;
        let mut combined_config: Mapping = serde_yaml::from_reader(config_file)
            .context("The homeserver.yaml generated by synapse is invalid")?;

        let mut insert_value = |key: &str, value: &str| {
            combined_config.insert(YAML::from(key), YAML::from(value));
        };
        insert_value("public_baseurl", &self.homeserver.public_baseurl);
        insert_value("server_name", &self.homeserver.server_name);
        insert_value(
            "registration_shared_secret",
            &self.homeserver.registration_shared_secret,
        );

        // HACK: Unless we're already overwriting listeners, patch `listeners`
        // to make sure it listens on the ports specified in `docker_config`.
        if self.homeserver.extra_fields.get("listeners").is_none() {
            if let Some(listeners) = combined_config.get_mut(&YAML::from("listeners")) {
                let listeners = listeners
                    .as_sequence_mut()
                    .ok_or_else(|| anyhow!("`listeners` should be a sequence"))?;
                debug!("Listeners: {:#?}", listeners);

                // FIXME: For the time being, let's only handle the simplest case.
                // If we're lucky, we'll be able to find WTH is going on that causes
                // Synapse to default to 9599 despite the fact that we're specifying 8008
                // everywhere.
                for listener in listeners {
                    let port = listener["port"]
                        .as_u64()
                        .ok_or_else(|| anyhow!("`listeners::port` should be a number"))?;
                    let found = self
                        .docker
                        .port_mapping
                        .iter()
                        .any(|mapping| mapping.guest as u64 == port);
                    if !found {
                        warn!("the generated dockerfile specifies port {}, but that port isn't opened", port);
                        warn!(
                            "replacing with port {} (mapped to {})",
                            self.docker.port_mapping[0].guest, self.docker.port_mapping[0].host
                        );
                        listener["port"] = self.docker.port_mapping[0].guest.into()
                    }
                }
            }
        }

        // Copy extra fields.
        for (key, value) in &self.homeserver.extra_fields {
            combined_config.insert(YAML::from(key.clone()), value.clone());
        }

        // Copy modules config.
        let modules_key = "modules".into();
        if !combined_config.contains_key(&modules_key)
            || combined_config.get(&modules_key).unwrap().is_null()
        {
            combined_config.insert(modules_key.clone(), YAML::Sequence(vec![]));
        }
        let modules_root = combined_config
            .get_mut(&modules_key)
            .unwrap() // Checked above
            .as_sequence_mut()
            .ok_or_else(|| anyhow!("In homeserver.yaml, expected a sequence for key `modules`"))?;
        for module in &self.modules {
            modules_root.push(module.config.clone());
        }
        let mut config_writer = LineWriter::new(std::fs::File::create(&target_path)?);
        config_writer
            .write_all(
                &serde_yaml::to_vec(&combined_config)
                    .context("Could not serialize combined homeserver config")?,
            )
            .context("Could not write combined homeserver config")?;
        Ok(())
    }

    /// The directory in which we're putting all data for this test.
    ///
    /// Cleaned up upon test start.
    pub fn test_root(&self) -> PathBuf {
        self.directories.root.join(&self.name)
    }

    /// The directory in which we're putting synapse data for this test.
    ///
    /// It will contain, among other things, the logs for the test.
    pub fn synapse_root(&self) -> PathBuf {
        self.test_root().join("synapse")
    }

    /// A tag for the Docker image we're creating/using.
    pub fn tag(&self) -> String {
        match self.synapse {
            SynapseVersion::Docker { ref tag } => {
                format!("mx-tester-synapse-{}-{}", tag, self.name)
            }
        }
    }

    pub fn network(&self) -> String {
        self.tag()
    }

    /// The name for the container we're using to setup Synapse.
    pub fn setup_container_name(&self) -> String {
        format!("mx-tester-synapse-setup-{}", self.name)
    }

    /// The name for the container we're using to actually run Synapse.
    pub fn run_container_name(&self) -> String {
        format!("mx-tester-synapse-run-{}", self.name)
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
    pub fn run(&self, env: &HashMap<&'static OsStr, OsString>) -> Result<(), Error> {
        debug!("Running with environment variables {:#?}", env);
        for line in &self.lines {
            let mut exec = ezexec::ExecBuilder::with_shell(line).map_err(|err| {
                anyhow!("Could not interpret `{}` as shell script: {}", line, err)
            })?;
            for (key, val) in env {
                exec.set_env(key, val);
            }
            exec.spawn_transparent()
                .map_err(|err| anyhow!("Error executing `{}`: {}", line, err))?
                .wait()
                .map_err(|err| anyhow!("Error during `{}`: {}", line, err))?;
        }
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
    data_dir: &Path,
    container_name: &str,
    cmd: Vec<String>,
    detach: bool,
) -> Result<(), Error> {
    let is_container_created = docker.is_container_created(container_name).await?;

    let env = vec![
        format!("SYNAPSE_SERVER_NAME={}", config.homeserver.server_name),
        "SYNAPSE_REPORT_STATS=no".into(),
        "SYNAPSE_CONFIG_DIR=/data".into(),
        format!(
            "SYNAPSE_HTTP_PORT={}",
            config
                .docker
                .port_mapping
                .get(0)
                .ok_or_else(|| anyhow!("In mx-tester.yml, an empty port mapping was specified"))?
                .guest
        ),
    ];

    if is_container_created {
        debug!("NO need to create container for {}", container_name);
    } else {
        debug!("We need to create container for {}", container_name);

        // Generate configuration to open and map ports.
        let host_port_bindings = config
            .docker
            .port_mapping
            .iter()
            .map(|mapping| {
                (
                    format!("{}/tcp", mapping.guest),
                    Some(vec![PortBinding {
                        host_port: Some(format!("{}", mapping.host)),
                        ..PortBinding::default()
                    }]),
                )
            })
            .collect();
        let exposed_ports = config
            .docker
            .port_mapping
            .iter()
            .map(|mapping| (format!("{}/tcp", mapping.guest), HashMap::new()))
            .collect();
        debug!("port_bindings: {:#?}", host_port_bindings);

        debug!(
            "containers: {:#?}",
            docker
                .list_containers(None::<ListContainersOptions<String>>)
                .await?
        );
        assert!(std::fs::metadata(data_dir).is_ok());

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
                            name: Some(RestartPolicyNameEnum::ALWAYS),
                            maximum_retry_count: None,
                        }),
                        // Extremely large memory allowance.
                        memory_reservation: Some(MEMORY_ALLOCATION_BYTES),
                        memory_swap: Some(-1),
                        // Mount guest directory `/data` into the host synapse data directory.
                        binds: Some(vec![format!(
                            "{}:/data:rw",
                            data_dir.as_os_str().to_string_lossy()
                        )]),
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
                    // Specify that `/data` may be mounted.
                    // The empty hashmap... is an oddity of the Docker Engine API.
                    volumes: Some(
                        vec![("/data".to_string(), HashMap::new())]
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

        let mut log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data_dir.join(format!("mx-tester-{}.log", container_name)))
            .await?;
        tokio::task::spawn(async move {
            debug!(target: "mx-tester-log", "Starting log watcher");
            while let Some(next) = logs.next().await {
                match next {
                    Ok(content) => {
                        debug!(target: "mx-tester-log", "{}", content);
                        log_file
                            .write_all(format!("{}", content).as_bytes())
                            .await?;
                    }
                    Err(err) => {
                        error!(target: "mx-tester-log", "{}", err);
                        log_file
                            .write_all(format!("ERROR: {}", err).as_bytes())
                            .await?;
                        return Err(err).context("Error in log");
                    }
                }
            }
            debug!(target: "mx-tester-log", "Stopped log watcher");
            Ok(())
        });
    }

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
        .start_exec(&exec.id, Some(StartExecOptions { detach }))
        .await
        .context("Error starting Synapse container")?;

    if !detach {
        let mut log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data_dir.join(format!("mx-tester-{}.out.log", container_name)))
            .await?;
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
                        log_file.write_all(format!("{}", output).as_bytes()).await?
                    }
                }
                bollard::exec::StartExecResults::Detached => panic!(),
            }
            debug!(target: "synapse", "Synapse container finished");
            Ok::<(), Error>(())
        })
        .await??;
    }

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

    // Remove any trace of a previous build. Ignore failures.
    let _ = docker.stop_container(&run_container_name, None).await;
    let _ = docker.remove_container(&run_container_name, None).await;
    let _ = docker.stop_container(&setup_container_name, None).await;
    let _ = docker.remove_container(&setup_container_name, None).await;
    let _ = docker.remove_image(config.tag().as_ref(), None, None).await;

    let synapse_root = config.synapse_root();
    let _ = std::fs::remove_dir_all(config.test_root());
    std::fs::create_dir_all(&synapse_root)
        .with_context(|| format!("Could not create directory {:#?}", synapse_root,))?;

    // Build modules
    let mut env = config.shared_env_variables()?;
    for module in &config.modules {
        let path = synapse_root.join(&module.name);
        env.insert(&*MX_TEST_MODULE_DIR, path.as_os_str().into());
        debug!(
            "Calling build script for module {} with MX_TEST_DIR={:#?}",
            &module.name, path
        );
        module
            .build
            .run(&env)
            .context("Error running build script")?;
        debug!("Completed one module.");
    }

    // Prepare Dockerfile including modules.
    let dockerfile_content = format!("
# A custom Dockerfile to rebuild synapse from the official release + plugins

FROM {docker_tag}

# Show the Synapse version, to aid with debugging.
RUN pip show matrix-synapse

# Copy and install custom modules.
RUN mkdir /mx-tester
{setup}
{copy}

VOLUME [\"/data\"]
ENTRYPOINT []
ENV SYNAPSE_HTTP_PORT=8008
EXPOSE 8008/tcp 8009/tcp 8448/tcp
",
    docker_tag = docker_tag,
    setup = config.modules.iter()
        .filter_map(|module| module.install.as_ref().map(|script| format!("## Setup {}\n{}\n", module.name, script.lines.iter().map(|line| format!("RUN {}", line)).format("\n"))))
        .format("\n"),
    copy = config.modules.iter()
        // FIXME: We probably want to test what happens with weird characters. Perhaps we'll need to somehow escape module.
        .map(|module| format!("COPY {module} /mx-tester/{module}\nRUN /usr/local/bin/python -m pip install /mx-tester/{module}", module=module.name))
        .format("\n"),
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
            let mut tar_builder = tar::Builder::new(tar_file);
            debug!("tar: adding directory {:#?}", synapse_root);
            tar_builder.append_dir_all("", &synapse_root)?;
            tar_builder.finish()?;
        }

        let tar_file = tokio::fs::File::open(&tar_path).await?;
        let stream = FramedRead::new(tar_file, BytesCodec::new());
        hyper::Body::wrap_stream(stream)
    };
    debug!("Building image with tag {}", config.tag());
    {
        let mut stream = docker.build_image(
            bollard::image::BuildImageOptions {
                pull: true,
                nocache: true,
                t: config.tag(),
                q: true,
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
                return Err(anyhow!(
                    "Error while building an image {}: {:#?}",
                    error,
                    info.error_detail
                ));
            }
            debug!("Build image progress {:#?}", info);
        }
    }
    debug!("Image built");
    Ok(())
}

/// Bring things up. Returns any environment variables to pass to the run script.
pub async fn up(docker: &Docker, config: &Config) -> Result<(), Error> {
    // This will break (on purpose) once we extend `SynapseVersion`.
    let SynapseVersion::Docker { .. } = config.synapse;

    // Create the network if necessary.
    // We'll add the container once it's available.
    let network_name = config.network();
    debug!("We'll need network {}", network_name);
    if !docker.is_network_up(&network_name).await? {
        debug!("Creating network {}", network_name);
        docker
            .create_network(CreateNetworkOptions {
                name: network_name.as_str(),
                ..CreateNetworkOptions::default()
            })
            .await?;
        assert!(
            docker.is_network_up(&network_name).await?,
            "The network should now be up"
        );
    } else {
        debug!("Network {} already exists", network_name);
    }

    // Only execute the `up` script once the network is up,
    // in case we want to e.g. bring up images that need
    // that same network.
    match config.up {
        Some(UpScript::FullUpScript(FullUpScript {
            before: Some(ref script),
            ..
        }))
        | Some(UpScript::SimpleScript(ref script)) => {
            let env = config.shared_env_variables()?;
            script
                .run(&env)
                .context("Error running `up` script (before)")?;
        }
        _ => {}
    }

    let setup_container_name = config.setup_container_name();
    let run_container_name = config.run_container_name();

    // Create the synapse data directory.
    // We'll use it as volume.
    let synapse_data_directory = config.synapse_root().join("data");
    std::fs::create_dir_all(&synapse_data_directory)
        .with_context(|| format!("Cannot create directory {:#?}", synapse_data_directory))?;

    // Cleanup leftovers.
    let homeserver_path = synapse_data_directory.join("homeserver.yaml");
    let _ = std::fs::remove_file(&homeserver_path);

    // Start a container to generate homeserver.yaml.
    start_synapse_container(
        docker,
        config,
        &synapse_data_directory,
        &setup_container_name,
        vec!["/start.py".to_string(), "generate".to_string()],
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
    while let Some(mapping) = config.docker.port_mapping.iter().find(|mapping| {
        debug!("Checking whether port {} is available", mapping.host);
        std::net::TcpListener::bind(("127.0.0.1", mapping.host as u16)).is_err()
    }) {
        debug!("Waiting until port {} is available", mapping.host);
        tokio::time::sleep(std::time::Duration::new(5, 0)).await;
    }
    debug!("Port is available, proceeding");
    start_synapse_container(
        docker,
        config,
        &synapse_data_directory,
        &run_container_name,
        vec!["/start.py".to_string()],
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
    //
    // This will presumably disappear if the `synapse_is_responsive` manipulation above works.
    match tokio::time::timeout(std::time::Duration::new(120, 0), async {
        handle_user_registration(config)
            .await
            .context("Failed to setup users")
    })
    .await
    {
        Err(_) => {
            // Timeout.
            panic!(
                "User registration is taking too long. {}",
                if docker.is_container_running(&run_container_name).await? {
                    "Container is running."
                } else {
                    "For some reason, Synapse has stopped. Please check the Synapse logs and/or rerun `mx-tester up`."
                }
            );
        }
        Ok(result) => result,
    }?;
    if let Some(UpScript::FullUpScript(FullUpScript {
        after: Some(ref script),
        ..
    })) = config.up
    {
        let env = config.shared_env_variables()?;
        script
            .run(&env)
            .context("Error running `up` script (after)")?;
    }
    Ok(())
}

/// Bring things down.
pub async fn down(docker: &Docker, config: &Config, status: Status) -> Result<(), Error> {
    // This will break (on purpose) once we extend `SynapseVersion`.
    let SynapseVersion::Docker { .. } = config.synapse;
    let run_container_name = config.run_container_name();

    if let Some(ref down_script) = config.down {
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
                .run(&env)
                .context("Error while running script `down/failure`"),
            (
                Status::Success,
                DownScript {
                    success: Some(ref on_success),
                    ..
                },
            ) => on_success
                .run(&env)
                .context("Error while running script `down/success`"),
            _ => Ok(()),
        };
        // Then run on_always.
        if let Some(ref on_always) = down_script.finally {
            on_always
                .run(&env)
                .context("Error while running script `down/finally`")?;
        }
        // Report any error from `on_failure` or `on_success`.
        result?
    }
    debug!(target: "mx-tester-down", "Taking down synapse.");
    match docker.stop_container(&run_container_name, None).await {
        Err(bollard::errors::Error::DockerResponseNotModifiedError { .. }) => {
            // Synapse is already down.
            debug!(target: "mx-tester-down", "Synapse was already down");
        }
        Err(bollard::errors::Error::DockerResponseNotFoundError { .. }) => {
            // Synapse is already down.
            debug!(target: "mx-tester-down", "No Synapse container");
        }
        Ok(_) => {
            debug!(target: "mx-tester-down", "Synapse taken down");
        }
        Err(err) => {
            return Err(err).context("Error stopping container");
        }
    }
    Ok(())
}

/// Run the testing script.
pub fn run(_docker: &Docker, config: &Config) -> Result<(), Error> {
    if let Some(ref code) = config.run {
        let env = config.shared_env_variables()?;
        code.run(&env).context("Error running `run` script")?;
    }
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
        debug!("is_network_up {:#?}", networks);
        Ok(networks.is_empty().not())
    }

    /// Check whether a container is currently running.
    async fn is_container_running(&self, name: &str) -> Result<bool, Error> {
        let containers = self
            .list_containers(Some(ListContainersOptions {
                // Check for running containers only
                all: false,
                // FIXME: This filter seems to filter by substring. That's... not reliable.
                filters: vec![("name", vec![name])].into_iter().collect(),
                ..ListContainersOptions::default()
            }))
            .await?;
        debug!("is_container_running {:#?}", containers);
        Ok(containers.is_empty().not())
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
        debug!("is_container_created {:#?}", containers);
        Ok(containers.is_empty().not())
    }
}
