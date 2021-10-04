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

use log::*;
use mx_tester::*;
use serde::Deserialize;

#[derive(Debug)]
enum Command {
    Build,
    Up,
    Run,
    Down,
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    /// A name for this test.
    name: String,

    /// Modules to install in Synapse.
    #[serde(default)]
    modules: Vec<ModuleConfig>,

    /// Values to pass through into the homserver.yaml for this synapse.
    homeserver_config: serde_yaml::Mapping,

    #[serde(default)]
    /// A script to run at the end of the setup phase.
    up: Option<Script>,

    #[serde(default)]
    /// The testing script to run.
    run: Option<Script>,

    #[serde(default)]
    /// A script to run at the start of the teardown phase.
    down: Option<DownScript>,

    /// Optional configuration to run a postgres container alongside synapse.
    postgres: Option<PostgresConfig>,
}

fn main() {
    use clap::*;
    env_logger::init();
    let matches = App::new("mx-tester")
        .version(std::env!("CARGO_PKG_VERSION"))
        .about("Command-line tool to simplify testing Matrix bots and Synapse modules")
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .default_value("mx-tester.yml")
                .help("The file containing the test configuration."),
        )
        .arg(
            Arg::with_name("command")
                .multiple(true)
                .takes_value(false)
                .possible_values(&["up", "run", "down", "build"])
                .help("The list of commands to run. Order matters and the same command may be repeated."),
        )
        .get_matches();

    let config_path = matches
        .value_of("config")
        .expect("Missing value for `config`");
    let config_file = std::fs::File::open(config_path)
        .unwrap_or_else(|err| panic!("Could not open config file `{}`: {}", config_path, err));

    let config: Config = serde_yaml::from_reader(config_file)
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

    // Now run the scripts.
    // We stop immediately if `build` or `up` fails but if `run` fails,
    // we may need to run some cleanup before stopping.
    //
    // FIXME: Is this the safest/least astonishing way of doing it?

    // Store the results of a `run` command in case it's followed by
    // a `down` command, which needs to decide between a success path
    // and a failure path.
    let mut result_run = None;
    for command in commands {
        match command {
            Command::Build => {
                build(&config.modules, SynapseVersion::ReleasedDockerImage)
                    .expect("Error in `build`");
            }
            Command::Up => {
                up_postgres(&config.postgres).expect("Could not start postgres.");
                up(
                    SynapseVersion::ReleasedDockerImage,
                    &config.up,
                    &config.homeserver_config,
                )
                .expect("Error in `up`");
            }
            Command::Run => {
                result_run = Some(run(&config.run));
            }
            Command::Down => {
                let status = match result_run {
                    None => Status::Manual,
                    Some(Ok(_)) => Status::Success,
                    Some(Err(_)) => Status::Failure,
                };
                let result_down = down(SynapseVersion::ReleasedDockerImage, &config.down, status);
                if let Some(result_run) = result_run {
                    result_run.expect("Error in `up`");
                }
                result_run = None;
                result_down.expect("Error during teardown");
            }
        }
    }
}
