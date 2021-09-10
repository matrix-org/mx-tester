use std::io::{ Error, ErrorKind };

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
        None => {},
        Some(ref down_script) => {
            // First run on_failure/on_success.
            // Store errors for later.
            let result = match (status, down_script) {
                (Status::Failure, DownScript {failure: Some(ref on_failure), ..}) => on_failure.run(),
                (Status::Success, DownScript {success: Some(ref on_success), ..}) => on_success.run(),
                _ => Ok(())
            };
            // Then run on_always.
            if let Some(ref on_always) = down_script.always {
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

/// The command requested by the user
#[derive(Deserialize, Clone, Copy)]
enum Command {
    #[serde(rename="up")]
    Up,
    #[serde(rename="down")]
    Down,
    #[serde(rename="run")]
    Run
}

/// The result of the test, as seen by `down()`.
enum Status {
    /// The test was a success.
    Success,

    /// The test was a failure.
    Failure,

    /// The test was not executed at all, we just ran `mx-tester down`.
    Manual
}

#[derive(Deserialize)]
#[serde(transparent)]
struct Script {
    /// The lines of the script.
    ///
    /// Passed without change to `std::process::Command`.
    ///
    /// To communicate with the script, clients should use
    /// an exchange file.
    lines: Vec<String>
}
impl Script {
    pub fn run(&self) -> Result<(), Error> {
        for line in &self.lines {
            let status = std::process::Command::new(&line)
                .spawn()?
                .wait()?;
            if !status.success() {
                return Err(Error::new(ErrorKind::InvalidData, format!("Error running command `{line}`: {status}", line = line, status = status)))
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct DownScript {
    /// Code to run in case the test is a success.
    success: Option<Script>,

    /// Code to run in case the test is a failure.
    failure: Option<Script>,

    /// Code to run regardless of the result of the test.
    ///
    /// Executed after `success` or `failure`.
    always: Option<Script>
}



#[derive(Deserialize)]
struct Config {
    /// If specified, a command to run if no command is passed.
    default_command: Option<Command>,

    /// A script to run at the end of the setup phase.
    up: Option<Script>,

    /// The testing script to run.
    run: Option<Script>,

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
                .global(true)
                .help("The file containing the test configuration, or mx-tester.yml if unspecified")
        )
        .subcommand(
            SubCommand::with_name("up")
        )
        .subcommand(
            SubCommand::with_name("down")
        )
        .subcommand(
            SubCommand::with_name("run")
        )
        .get_matches();

    let config_path = matches.value_of("config")
        .expect("Missing value for `config`");
    let config_file = std::fs::File::open(config_path)
        .unwrap_or_else(|err| panic!("Could not open config file `{}`: {}", config_path, err));

    let config: Config = serde_yaml::from_reader(config_file)
        .unwrap_or_else(|err| panic!("Invalid config file `{}`: {}", config_path, err));

    let command = match matches.subcommand_name() {
        None => config.default_command.unwrap_or(Command::Run),
        Some("run") => Command::Run,
        Some("down") => Command::Down,
        Some("up") => Command::Up,
        _ => unreachable!()
    };

    match command {
        Command::Up => up(&config.up)
            .unwrap(),
        Command::Down => down(&config.down, Status::Manual)
            .unwrap(),
        Command::Run => {
            // Always run the setup.
            up(&config.up).expect("Error during setup");

            // Run the test.
            let result = run(&config.run);
            if result.is_err() {
                warn!("Encountered an error during the test.");
            }
            let status = if result.is_ok() { Status::Success } else { Status::Failure };
            let result_2 = down(&config.down, status);
            result.expect("Error during test");
            result_2.expect("Error during teardown");
        }
    }
}
