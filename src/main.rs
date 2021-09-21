use log::*;
use mx_tester::*;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Default)]
struct Commands {
    /// If `true`, execute build scripts.
    build: bool,

    /// If `true`, execute up scripts.
    up: bool,

    /// If `true`, execute run scripts.
    run: bool,

    /// If `true`, execute down scripts.
    down: bool,
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
            build: false,
            up: true,
            run: true,
            down: true,
        },
        Some(c) => {
            let mut commands = Commands::default();
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
                    "build" => {
                        commands.build = true;
                    }
                    _ => panic!("Invalid command `{}`", command),
                }
            }
            commands
        }
    };
    debug!("Running {:?}", commands);

    // Now run the scripts.
    // We stop immediately if `build` or `up` fails but if `run` fails,
    // we may need to run some cleanup before stopping.
    //
    // FIXME: Is this the safest/least astonishing way of doing it?

    if commands.build {
        build(&config.modules, SynapseVersion::ReleasedDockerImage)
            .expect("Error while building image");
    }
    if commands.up {
        up(
            SynapseVersion::ReleasedDockerImage,
            &config.up,
            config.homeserver_config,
        )
        .expect("Error during setup");
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
        down(SynapseVersion::ReleasedDockerImage, &config.down, status)
    } else {
        Ok(())
    };

    result_run.expect("Error during test");
    result_down.expect("Error during teardown");
}
