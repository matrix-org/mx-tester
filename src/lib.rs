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

use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::{OsStr, OsString},
    io::{Error, ErrorKind, LineWriter, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use itertools::Itertools;
use lazy_static::lazy_static;
use log::{debug, warn};
use serde::Deserialize;

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

    /// The docker tag used for the Synapse image we produce.
    static ref PATCHED_IMAGE_DOCKER_TAG: OsString = OsString::from_str("mx-tester/synapse").unwrap();
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

pub enum SynapseVersion {
    /// The latest version of Synapse released on https://hub.docker.com/r/matrixdotorg/synapse/
    ReleasedDockerImage,
    // FIXME: Allow using a version of Synapse that lives in a local directory
    // (this will be sufficient to also implement pulling from github develop)
}
impl SynapseVersion {
    pub fn tag(&self) -> Cow<'static, OsStr> {
        let tag: &'static OsStr = PATCHED_IMAGE_DOCKER_TAG.as_ref();
        tag.into()
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
    /// Substitute anything that looks like the use of environment variable such as $A or ${B} using the entries to the hash map provided.
    /// If there is no matching table entry for $FOO then it will remain as $FOO in the script line.
    fn substitute_env_vars<'a>(
        line: &'a str,
        env: &HashMap<&'static OsStr, OsString>,
    ) -> Cow<'a, str> {
        shellexpand::env_with_context_no_errors(line, |str| match env.get(OsStr::new(str)) {
            Some(value) => value.to_str(),
            _ => None,
        })
    }
    /// Parse a line of script into a std::process::Command and its arguments.
    /// Returns None when there is no command token in the line (e.g. just whitespace).
    fn parse_command(&self, line: &str) -> Option<std::process::Command> {
        let tokens = comma::parse_command(line)?;
        let mut token_stream = tokens.iter();
        let mut command = std::process::Command::new(OsString::from(token_stream.next()?));
        for token in token_stream {
            command.arg(token);
        }
        Some(command)
    }
    pub fn run(&self, env: &HashMap<&'static OsStr, OsString>) -> Result<(), Error> {
        for line in &self.lines {
            let line = Script::substitute_env_vars(line, env);
            let mut command = match self.parse_command(&line) {
                Some(command) => command,
                None => {
                    warn!("Skipping empty line in script {:?}", self.lines);
                    continue;
                }
            };
            let status = command.envs(env).spawn()?.wait()?;
            if !status.success() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "Error running command `{line}`: {status}",
                        line = line,
                        status = status
                    ),
                ));
            }
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
    build: Script,
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

/// Create a map containing the environment variables that are common
/// to all scripts.
///
/// Callers may add additional variables that are specific to a given
/// script step.
fn shared_env_variables() -> Result<HashMap<&'static OsStr, OsString>, Error> {
    let synapse_root = synapse_root();
    let script_tmpdir = std::env::temp_dir().join("mx-tester").join("scripts");
    std::fs::create_dir_all(&script_tmpdir).map_err(|err| {
        Error::new(
            err.kind(),
            format!(
                "Could not create directory {:?}: {}",
                script_tmpdir.as_os_str(),
                err
            ),
        )
    })?;
    let curdir = std::env::current_dir()?;
    let mut env: HashMap<&'static OsStr, _> = HashMap::new();
    env.insert(&*MX_TEST_SYNAPSE_DIR, synapse_root.as_os_str().into());
    env.insert(&*MX_TEST_SCRIPT_TMPDIR, script_tmpdir.as_os_str().into());
    env.insert(&*MX_TEST_CWD, curdir.as_os_str().into());
    Ok(env)
}

fn synapse_root() -> PathBuf {
    std::env::temp_dir().join("mx-tester").join("synapse")
}

/// Rebuild the Synapse image with modules.
pub fn build(config: &[ModuleConfig], version: SynapseVersion) -> Result<(), Error> {
    let synapse_root = synapse_root();
    std::fs::create_dir_all(&synapse_root)
        .unwrap_or_else(|err| panic!("Cannot create directory {:?}: {}", synapse_root, err));
    // Build modules
    let mut env = shared_env_variables()?;
    for module in config {
        let path = synapse_root.join(&module.name);
        env.insert(&*MX_TEST_MODULE_DIR, path.as_os_str().into());
        debug!(
            "Calling build script for module {} with MX_TEST_DIR={:?}",
            &module.name,
            path.to_str().unwrap()
        );
        module.build.run(&env)?;
        debug!("Completed one module.");
    }

    // Prepare Dockerfile including modules.
    let dockerfile_content = format!("
# A custom Dockerfile to rebuild synapse from the official release + plugins

FROM matrixdotorg/synapse:latest

# We need gcc to build pyahocorasick
RUN apt-get update --quiet && apt-get install gcc --yes --quiet

# Show the Synapse version, to aid with debugging.
RUN pip show matrix-synapse

# Copy and install custom modules.
RUN mkdir /mx-tester
{copy}

VOLUME [\"/data\"]

EXPOSE 8008/tcp 8009/tcp 8448/tcp
",
    copy = config.iter()
        // FIXME: We probably want to test what happens with weird characters. Perhaps we'll need to somehow escape module.
        .map(|module| format!("COPY {module} /mx-tester/{module}\nRUN /usr/local/bin/python -m pip install /mx-tester/{module}", module=module.name))
        .format("\n")
);
    debug!("dockerfile {}", dockerfile_content);

    let docker_dir_path = std::env::temp_dir().join("mx-tester").join("docker");
    std::fs::create_dir_all(&docker_dir_path).unwrap_or_else(|err| {
        panic!(
            "Could not create directory `{:?}`: {}",
            &docker_dir_path, err
        )
    });
    let dockerfile_path = docker_dir_path.join("Dockerfile");
    std::fs::write(&dockerfile_path, dockerfile_content)
        .unwrap_or_else(|err| panic!("Could not write file `{:?}`: {}", &dockerfile_path, err));

    debug!("Building image with tag {:?}", version.tag());
    std::process::Command::new("docker")
        .arg("build")
        .args(["--pull", "--no-cache"])
        .arg("-t")
        .arg(version.tag())
        .arg("-f")
        .arg(&dockerfile_path)
        .arg(&synapse_root)
        .output()
        .expect("Could not launch image rebuild");

    Ok(())
}

/// Generate the data directory and default synapse configuration.
fn generate(synapse_data_directory: &Path) -> Result<(), Error> {
    // FIXME: I think we're creating tonnes of unnamed garbage containers each time we run this.
    let mut command = std::process::Command::new("docker");
    command
        .arg("run")
        .arg("-e")
        // FIXME: Use server name from config.
        .arg("SYNAPSE_SERVER_NAME=localhost:8080")
        .arg("-e")
        .arg("SYNAPSE_REPORT_STATS=no")
        .arg("-e")
        .arg("SYNAPSE_CONFIG_DIR=/data");
    // Ensure that the config files and media can be deleted by the user
    // who launched the program by giving synapse the same uid/gid.
    #[cfg(unix)]
    command
        .arg("-e")
        .arg(format!("UID={}", nix::unistd::getuid()))
        .arg("-e")
        .arg(format!("GID={}", nix::unistd::getegid()));
    let output = command
        .arg("-p")
        .arg("9999:8080")
        .arg("-v")
        .arg(format!(
            "{}:/data",
            &synapse_data_directory.to_str().unwrap()
        ))
        .arg(&*PATCHED_IMAGE_DOCKER_TAG)
        .arg("generate")
        .output()
        .expect("Could not generate synapse files");
    debug!(
        "generate missing config: {}\n{}",
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap()
    );
    Ok(())
}

/// Raise an image.
fn up_image(synapse_data_directory: &Path, create_new_container: bool) -> Result<(), Error> {
    let container_name = "mx-tester_synapse";
    let container_up = is_container_up(container_name);
    if container_up && create_new_container {
        container_stop(container_name)
    } else if container_up {
        return Ok(());
    }
    if is_container_built(container_name) {
        container_rm(container_name)
    }
    let mut command = std::process::Command::new("docker");
    command.arg("run");
    // Ensure that the config files and media can be deleted by the user
    // who launched the program by giving synapse the same uid/gid.
    #[cfg(unix)]
    command
        .arg("-e")
        .arg(format!("UID={}", nix::unistd::getuid()))
        .arg("-e")
        .arg(format!("GID={}", nix::unistd::getegid()));
    command
        .arg("--detach")
        .arg("--name")
        .arg("mx-tester_synapse")
        .arg("-p")
        .arg("9999:9999")
        .arg("-v")
        .arg(format!(
            "{}:{}",
            &synapse_data_directory.to_str().unwrap(),
            "/data"
        ))
        .arg(&*PATCHED_IMAGE_DOCKER_TAG);
    let output = command.output().expect("Could not start image");
    debug!(
        "up_image {:?}: {}\n{}",
        &*PATCHED_IMAGE_DOCKER_TAG,
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap()
    );
    Ok(())
}

/// Check whether the named container is currently up.
fn is_container_up(container_name: &str) -> bool {
    let mut command = std::process::Command::new("docker");
    command
        .arg("container")
        .arg("ps")
        .arg("--no-trunc")
        .arg("--filter")
        .arg(format!("name={}", container_name));
    let output = command
        .output()
        .unwrap_or_else(|_| panic!("Could not check if container name={} is up", container_name));
    debug!(
        "is_container_up name={} output: {:?}",
        container_name, output
    );
    let all_output =
        String::from_utf8(output.stdout).expect("Invalid output from docker container ps.");
    all_output.contains(container_name)
}

/// Check whether a container with this name has been built already.
fn is_container_built(container_name: &str) -> bool {
    let mut command = std::process::Command::new("docker");
    command
        .arg("container")
        .arg("ls")
        .arg("-a")
        .arg("--no-trunc")
        .arg("--filter")
        .arg(format!("name={}", container_name));
    let output = command.output().unwrap_or_else(|_| {
        panic!(
            "Could not check if container name={} exists",
            container_name
        )
    });
    debug!(
        "is_container_built name={} output: {:?}",
        container_name, output
    );
    let all_output =
        String::from_utf8(output.stdout).expect("Invalid output from docker container ls.");
    all_output.contains(container_name)
}

/// Remove the named container.
fn container_rm(container_name: &str) {
    let mut command = std::process::Command::new("docker");
    command
        .arg("container")
        .arg("rm")
        .arg(container_name)
        .output()
        .unwrap_or_else(|_| panic!("Could not remove container: {}", container_name));
}

/// Bring things up. Returns any environment variables to pass to the run script.
pub fn up(
    version: SynapseVersion,
    script: &Option<Script>,
    homeserver_config: serde_yaml::Mapping,
) -> Result<(), Error> {
    let synapse_data_directory = synapse_root().join("data");
    std::fs::create_dir_all(&synapse_data_directory).unwrap_or_else(|err| {
        panic!(
            "Cannot create directory {:?}: {}",
            synapse_data_directory, err
        )
    });
    debug!("generating synapse data");
    generate(&synapse_data_directory)?;
    debug!("done generating");
    // Apply config from mx-tester.yml to the homeserver.yaml that was just made
    update_homeserver_config_with_config(
        &synapse_data_directory.join("homeserver.yaml"),
        homeserver_config,
    );
    // FIXME: Allow configuration of recreating container if the image has been rebuilt.
    up_image(&synapse_data_directory, false)?;
    // FIXME: If we have a token for an admin user, test it.
    // FIXME: Where should we store the token for the admin user? File storage? An embedded db?
    // FIXME: Note that we need to wait and retry, as bringing up Synapse can take a little time.
    // FIXME: If we have no token or the token is invalid, create an admin user.

    if let Some(ref script) = script {
        let env = shared_env_variables()?;
        script.run(&env)?;
    }
    Ok(())
}

/// Stop a container.
pub fn container_stop(container_name: &str) {
    let status = std::process::Command::new("docker")
        .arg("stop")
        .arg(container_name)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .output()
        .expect("Could not take down the synapse container");
}

/// Bring things down.
pub fn down(
    version: SynapseVersion,
    script: &Option<DownScript>,
    status: Status,
) -> Result<(), Error> {
    if let Some(ref down_script) = *script {
        let env = shared_env_variables()?;
        // First run on_failure/on_success.
        // Store errors for later.
        let result = match (status, down_script) {
            (
                Status::Failure,
                DownScript {
                    failure: Some(ref on_failure),
                    ..
                },
            ) => on_failure.run(&env),
            (
                Status::Success,
                DownScript {
                    success: Some(ref on_success),
                    ..
                },
            ) => on_success.run(&env),
            _ => Ok(()),
        };
        // Then run on_always.
        if let Some(ref on_always) = down_script.finally {
            on_always.run(&env)?;
        }
        // Report any error from `on_failure` or `on_success`.
        result?
    }
    debug!("Taking down synapse.");
    container_stop("mx-tester_synapse");
    Ok(())
}

/// Run the testing script.
pub fn run(script: &Option<Script>) -> Result<(), Error> {
    if let Some(ref code) = script {
        let env = shared_env_variables()?;
        // FIXME: Load the token, etc. from disk storage.
        code.run(&env)?;
    }
    Ok(())
}

/// Update the homserver.yaml at the given path (usually one that has been generated by synapse)
/// with the properties in the provided serde_yaml::Mapping (which will usually be provided from mx-tester.yaml)
fn update_homeserver_config_with_config(
    target_homeserver_config: &Path,
    homeserver_config: serde_yaml::Mapping,
) {
    let config_file = std::fs::File::open(target_homeserver_config).unwrap_or_else(|err| {
        panic!(
            "Could not open the homeserver.yaml that was generated by synapse `{:?}`: {}",
            target_homeserver_config, err
        )
    });

    let mut combined_config: serde_yaml::Mapping = serde_yaml::from_reader(config_file)
        .unwrap_or_else(|err| {
            panic!(
                "The homeserver.yaml generated by synapse is invalid `{:?}`: {}",
                target_homeserver_config, err
            )
        });

    for (key, value) in homeserver_config {
        combined_config.insert(key, value);
    }
    let mut config_writer = LineWriter::new(
        std::fs::File::create(&target_homeserver_config)
            .expect("Could not write to the homeserver.yaml that was generated by synapse"),
    );
    config_writer
        .write_all(
            &serde_yaml::to_vec(&combined_config)
                .expect("Could not serialize combined homeserver config"),
        )
        .expect("Could not overwrite the homeserver.yaml that was generated by synapse.");
}
