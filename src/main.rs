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

use anyhow::Context;
use log::*;
use mx_tester::*;

#[derive(Debug)]
enum Command {
    Build,
    Up,
    Run,
    Down,
}

#[tokio::main]
async fn main() {
    use clap::*;
    env_logger::init();
    let matches = App::new("mx-tester")
        .version(std::env!("CARGO_PKG_VERSION"))
        .about("Command-line tool to simplify testing Matrix bots and Synapse modules")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .default_value("mx-tester.yml")
                .help("The file containing the test configuration."),
        )
        .arg(
            Arg::new("command")
                .multiple_occurrences(true)
                .takes_value(false)
                .possible_values(&["up", "run", "down", "build"])
                .help("The list of commands to run. Order matters and the same command may be repeated."),
        )
        .arg(
            Arg::new("username")
                .short('u')
                .long("username")
                .takes_value(true)
                .required(false)
                .help("A username for logging to the Docker registry")
        )
        .arg(
            Arg::new("password")
                .short('p')
                .long("password")
                .takes_value(true)
                .required(false)
                .help("A password for logging to the Docker registry")
        )
        .arg(
            Arg::new("server")
                .long("server")
                .takes_value(true)
                .required(false)
                .help("A server name for the Docker registry")
        )
        .arg(
            Arg::new("root_dir")
                .long("root")
                .value_name("PATH")
                .takes_value(true)
                .required(false)
                .help("Write all files in subdirectories of this directory (default: /tmp)")
        )
        .arg(
            Arg::new("workers")
                .long("workers")
                .takes_value(false)
                .required(false)
                .help("If specified, use workerized Synapse (default: none)")
        )
        .arg(
            Arg::new("synapse-tag")
                .long("synapse-tag")
                .value_name("TAG")
                .takes_value(true)
                .required(false)
                .help("If specified, use the Docker image published with TAG (default: use mx-tester.yml or tag `latest`)")
        )
        .arg(
            Arg::new("no-autoclean-on-error")
                .long("no-autoclean-on-error")
                .takes_value(false)
                .help("If specified, do NOT clean up containers in case of error")
        )
        .get_matches();

    let config_path = matches
        .value_of("config")
        .expect("Missing value for `config`");
    let config_file = std::fs::File::open(config_path)
        .unwrap_or_else(|err| panic!("Could not open config file `{}`: {}", config_path, err));

    let mut config: Config = serde_yaml::from_reader(config_file)
        .unwrap_or_else(|err| panic!("Invalid config file `{}`: {}", config_path, err));
    debug!("Config: {:2?}", config);

    let commands = match matches.values_of("command") {
        None => vec![Command::Up, Command::Run, Command::Down],
        Some(values) => values
            .map(|command| match command {
                "up" => Command::Up,
                "down" => Command::Down,
                "run" => Command::Run,
                "build" => Command::Build,
                _ => panic!("Invalid command `{}`", command),
            })
            .collect(),
    };
    debug!("Running {:?}", commands);
    debug!("Root: {:?}", config.test_root());

    if let Some(server) = matches.value_of("server") {
        config.credentials.serveraddress = Some(server.to_string());
    }
    if let Some(password) = matches.value_of("password") {
        config.credentials.password = Some(password.to_string());
    }
    if let Some(username) = matches.value_of("username") {
        config.credentials.username = Some(username.to_string());
    }
    if let Some(root) = matches.value_of("root_dir") {
        config.directories.root = std::path::Path::new(root).to_path_buf()
    }
    let workers = matches.is_present("workers");
    config.workers = workers;
    if let Some(synapse_tag) = matches.value_of("synapse-tag") {
        config.synapse = SynapseVersion::Docker {
            tag: format!("matrixdotorg/synapse:{}", synapse_tag)
        };
    }

    // Now run the scripts.
    // We stop immediately if `build` or `up` fails but if `run` fails,
    // we may need to run some cleanup before stopping.

    if commands.is_empty() {
        // No need to initialize Docker.
        return;
    }

    let docker = if let Some(ref server) = config.credentials.serveraddress {
        // If we have provided a server, well, let's use it.
        // This is mainly useful for running in CI.
        info!("Using docker repository {}", server);
        bollard::Docker::connect_with_http_defaults().context("Connecting with http defaults")
    } else {
        // Otherwise, use the local defaults.
        info!("Using local docker repository");
        bollard::Docker::connect_with_local_defaults().context("Connecting with local defaults")
    }
    .expect("Failed to connect to the Docker daemon");

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
                result_run = Some(run(&docker, &config));
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
}
