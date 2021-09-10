use std::io::{Error, ErrorKind};

use log::*;
use serde::Deserialize;

/// Bring things up.
fn up(script: &Option<Script>) -> Result<(), Error> {
    // FIXME: If necessary, rebuild Synapse.
    // FIXME: Up Synapse.
    // FIXME: If we have a token for an admin user, test it.
    // FIXME: If we have no token or it doesn't work, create an admin user.
    // FIXME: Write down synapse information in a yaml file.
    // FIXME: If the configuration states that we need to run an `up` script, run it.
    unimplemented!()
}

/// Bring things down.
fn down(script: &Option<DownScript>, status: Status) -> Result<(), Error> {
    match *script {
        None => {}
        Some(ref down_script) => {
            // First run on_failure/on_success.
            // Store errors for later.
            let result = match (status, down_script) {
                (
                    Status::Failure,
                    DownScript {
                        failure: Some(ref on_failure),
                        ..
                    },
                ) => on_failure.run(),
                (
                    Status::Success,
                    DownScript {
                        success: Some(ref on_success),
                        ..
                    },
                ) => on_success.run(),
                _ => Ok(()),
            };
            // Then run on_always.
            if let Some(ref on_always) = down_script.finally {
                on_always.run()?;
            }
            // Report any error from `on_failure` or `on_success`.
            result?
        }
    }
    // FIXME: Bring down Synapse.
    unimplemented!()
}

/// Run the testing script.
fn run(script: &Option<Script>) -> Result<(), Error> {
    if let Some(ref code) = script {
        code.run()?;
    }
    Ok(())
}

#[derive(Debug)]
struct Commands {
    up: bool,
    run: bool,
    down: bool,
}

/// The result of the test, as seen by `down()`.
enum Status {
    /// The test was a success.
    Success,

    /// The test was a failure.
    Failure,

    /// The test was not executed at all, we just ran `mx-tester down`.
    Manual,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct Script {
    /// The lines of the script.
    ///
    /// Passed without change to `std::process::Command`.
    ///
    /// To communicate with the script, clients should use
    /// an exchange file.
    lines: Vec<String>,
}
impl Script {
    pub fn run(&self) -> Result<(), Error> {
        for line in &self.lines {
            let status = std::process::Command::new(&line).spawn()?.wait()?;
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

#[derive(Debug, Deserialize)]
struct DownScript {
    /// Code to run in case the test is a success.
    success: Option<Script>,

    /// Code to run in case the test is a failure.
    failure: Option<Script>,

    /// Code to run regardless of the result of the test.
    ///
    /// Executed after `success` or `failure`.
    finally: Option<Script>,
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    #[serde(default)]
    /// A script to run at the end of the setup phase.
    up: Option<Script>,

    #[serde(default)]
    /// The testing script to run.
    run: Option<Script>,

    #[serde(default)]
    /// A script to run at the start of the teardown phase.
    down: Option<DownScript>,
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
                .possible_values(&["up", "run", "down"])
                .help("The list of commands to run. Order is ignored."),
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
        None => Commands {
            up: true,
            run: true,
            down: true,
        },
        Some(c) => {
            let mut commands = Commands {
                up: false,
                run: false,
                down: false,
            };
            for command in c {
                match command {
                    "up" => {
                        commands.up = true;
                    }
                    "down" => {
                        commands.down = true;
                    }
                    "run" => {
                        commands.run = true;
                    }
                    _ => panic!("Invalid command `{}`", command),
                }
            }
            commands
        }
    };
    debug!("Running {:?}", commands);

    // Now run the scripts.
    // We stop immediately if `up` fails but if `run` fails,
    // we may need to run some cleanup before stopping.
    //
    // FIXME: Is this the safest/least astonishing way of doing it?
    if commands.up {
        up(&config.up).expect("Error during setup");
    };

    let result_run = if commands.run {
        run(&config.run)
    } else {
        Ok(())
    };
    let result_down = if commands.down {
        let status = match (commands.run, &result_run) {
            (false, _) => Status::Manual,
            (_, &Ok(_)) => Status::Success,
            (_, &Err(_)) => Status::Failure,
        };
        down(&config.down, status)
    } else {
        Ok(())
    };

    result_run.expect("Error during test");
    result_down.expect("Error during teardown");
}
