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

use std::borrow::Cow;

use anyhow::Context;
use clap::command;
use log::*;
use mx_tester::*;

const CONFIG_PATH_AUTOTEST: &str = "[empty]";

#[derive(Debug)]
enum Command {
    Build,
    Up,
    Run,
    Down,
}

#[tokio::main]
async fn main() {
    use clap::Arg;
    env_logger::init();
    let matches = command!()
        .version(std::env!("CARGO_PKG_VERSION"))
        .about("Command-line tool to simplify testing Matrix bots and Synapse modules")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .global(true)
                .default_value("mx-tester.yml")
                .help("The file containing the test configuration. Pass `[empty]` to run an empty mx-tester.yml, for self-testing."),
        )
        .arg(
            // Note: `multiple_ocurences` is deprecated but `ArgAction::Append` doesn't actually replace it.
            #[allow(deprecated)]
            Arg::new("command")
                .action(clap::ArgAction::Append)
                .takes_value(false)
                .multiple_occurrences(true)
                .value_parser(["up", "run", "down", "build"])
                .help("The list of commands to run. Order matters and the same command may be repeated."),
        )
        .arg(
            Arg::new("username")
                .short('u')
                .long("username")
                .global(true)
                .takes_value(true)
                .required(false)
                .help("A username for logging to the Docker registry")
        )
        .arg(
            Arg::new("password")
                .short('p')
                .long("password")
                .global(true)
                .takes_value(true)
                .required(false)
                .help("A password for logging to the Docker registry")
        )
        .arg(
            Arg::new("server")
                .long("server")
                .global(true)
                .takes_value(true)
                .required(false)
                .help("A server name for the Docker registry")
        )
        .arg(
            Arg::new("root_dir")
                .long("root")
                .global(true)
                .value_name("PATH")
                .takes_value(true)
                .required(false)
                .help("Write all files in subdirectories of this directory (default: /tmp)")
        )
        .arg(
            Arg::new("workers")
                .long("workers")
                .global(true)
                .takes_value(false)
                .required(false)
                .help("If specified, use workerized Synapse (default: no workers). If you have run `build` with `--workers`, make sure that `up` and `build` are also run with `--workers`.")
        )
        .arg(
            Arg::new("synapse-tag")
                .long("synapse-tag")
                .global(true)
                .value_name("TAG")
                .takes_value(true)
                .required(false)
                .help("If specified, use the Docker image published with TAG (default: use mx-tester.yml or tag `latest`)")
        )
        .arg(
            Arg::new("no-autoclean-on-error")
                .long("no-autoclean-on-error")
                .global(true)
                .takes_value(false)
                .help("If specified, do NOT clean up containers in case of error")
        )
        .arg(
            Arg::new("docker-ssl")
                .long("docker-ssl")
                .global(true)
                .default_value("detect")
                .value_parser(["always", "never", "detect"])
                .help("If `detect`, attempt to auto-detect a SSL configuration and fallback tp HTTP otherwise. This may be broken in your CI. If `always`, fail if there is no Docker SSL configuration. If `never`, ignore any Docker SSL configuration.")
        )
         .get_matches();
    let config_path: &String = matches
        .get_one("config")
        .expect("Missing value for `config`");
    let is_self_test = config_path == CONFIG_PATH_AUTOTEST;

    let commands = match matches.get_many::<String>("command") {
        None if is_self_test => vec![],
        None => vec![Command::Up, Command::Run, Command::Down],
        Some(values) => values
            .map(|command| match command.as_ref() {
                "up" => Command::Up,
                "down" => Command::Down,
                "run" => Command::Run,
                "build" => Command::Build,
                _ => panic!("Invalid command `{}`", command),
            })
            .collect(),
    };
    debug!("Running {:?}", commands);

    let mut config = {
        if is_self_test {
            Config::builder()
                .name("mx-tester-autotest".to_string())
                .build()
        } else {
            let config_file = std::fs::File::open(config_path).unwrap_or_else(|err| {
                panic!("Could not open config file `{}`: {}", config_path, err)
            });
            serde_yaml::from_reader(config_file)
                .unwrap_or_else(|err| panic!("Invalid config file `{}`: {}", config_path, err))
        }
    };
    debug!("Config: {:2?}", config);
    for (key, value) in std::env::vars().filter(|(key, _)| key.starts_with("DOCKER_")) {
        debug!("{}={}", key, value);
    }
    debug!("Root: {:?}", config.test_root());

    if let Some(server) = matches.get_one::<String>("server") {
        config.credentials.serveraddress = Some(server.to_string());
    }
    if let Some(password) = matches.get_one::<String>("password") {
        config.credentials.password = Some(password.to_string());
    }
    if let Some(username) = matches.get_one::<String>("username") {
        config.credentials.username = Some(username.to_string());
    }
    if let Some(root) = matches.get_one::<String>("root_dir") {
        config.directories.root = std::path::Path::new(root).to_path_buf()
    }
    let workers = matches.contains_id("workers");
    config.workers.enabled = workers;
    if let Some(synapse_tag) = matches.get_one::<String>("synapse-tag") {
        config.synapse = SynapseVersion::Docker {
            tag: format!("matrixdotorg/synapse:{}", synapse_tag),
        };
    }

    enum ShouldSsl {
        Never,
        Detect,
        Always,
    }
    let should_ssl = match matches.get_one::<String>("docker-ssl").unwrap().as_ref() {
        "never" => ShouldSsl::Never,
        "detect" => ShouldSsl::Detect,
        "always" => ShouldSsl::Always,
        _ => panic!(), // This should be caught by Clap
    };

    // Now run the scripts.
    // We stop immediately if `build` or `up` fails but if `run` fails,
    // we may need to run some cleanup before stopping.

    if !is_self_test && commands.is_empty() {
        // No need to initialize Docker.
        return;
    }

    println!(
        "mx-tester {version} starting. Logs will be stored at {logs_dir:?}",
        version = env!("CARGO_PKG_VERSION"),
        logs_dir = config.logs_dir()
    );
    let has_docker_cert_path = std::env::var("DOCKER_CERT_PATH").is_ok();
    let mut docker = match (should_ssl, &config.credentials.serveraddress, has_docker_cert_path) {
        // No server configured => we can only run locally.
        (ShouldSsl::Never, None, _) | (ShouldSsl::Detect, None, _) => {
            info!("Using local docker repository");
            bollard::Docker::connect_with_local_defaults().context("Connecting with local defaults")    
        }
        (ShouldSsl::Always, None, _) => {
            panic!("Option conflict: `--docker-ssl=always` requires option `--server` or an server address in mx-tester.yml")
        }
        // Server configured => we can run either with HTTP or SSL.
        (ShouldSsl::Never, &Some(ref server), _) | (ShouldSsl::Detect, &Some(ref server), false) => {
            info!("Using docker repository with HTTP {}", server);
            bollard::Docker::connect_with_http_defaults().context("Connecting with HTTP")            
        },
        (ShouldSsl::Always, &Some(ref server), _) | (ShouldSsl::Detect, &Some(ref server), true) => {
            info!("Using docker repository with SSL {}", server);
            bollard::Docker::connect_with_ssl_defaults().context("Connecting with SSL")
        }
    }.expect("Failed to connect to the Docker daemon");
    docker.set_timeout(std::time::Duration::from_secs(600));

    // Test that we can connect to Docker.
    let version = docker
        .version()
        .await
        .expect("Checking connection to docker daemon");
    println!(
        "Using docker {}",
        version.version.map(Cow::from).unwrap_or_else(|| "?".into())
    );

    // Store the results of a `run` command in case it's followed by
    // a `down` command, which needs to decide between a success path
    // and a failure path.
    let mut result_run = None;
    for command in commands {
        match command {
            Command::Build => {
                info!("mx-tester build...");
                build(&docker, &config).await.expect("Error in `build`");
            }
            Command::Up => {
                info!("mx-tester up...");
                up(&docker, &config).await.expect("Error in `up`");
            }
            Command::Run => {
                info!("mx-tester run...");
                result_run = Some(run(&docker, &config).await);
            }
            Command::Down => {
                info!("mx-tester down...");
                let status = match result_run {
                    None => Status::Manual,
                    Some(Ok(_)) => Status::Success,
                    Some(Err(_)) => Status::Failure,
                };
                let result_down = down(&docker, &config, status).await;
                if let Some(result_run) = result_run.take() {
                    // Display errors due to `run` before errors due to `down`.
                    result_run.expect("Error in `run`");
                }
                result_down.expect("Error during teardown");
            }
        }
    }
    if let Some(result) = result_run {
        // We haven't consumed the result of run().
        result.expect("Error in `run`");
    }
    println!("* mx-tester success");
}
