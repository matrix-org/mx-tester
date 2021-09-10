use std::io::{Error, ErrorKind};

use serde::Deserialize;


/// The result of the test, as seen by `down()`.
pub enum Status {
    /// The test was a success.
    Success,

    /// The test was a failure.
    Failure,

    /// The test was not executed at all, we just ran `mx-tester down`.
    Manual,
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


/// Bring things up.
pub fn up(script: &Option<Script>) -> Result<(), Error> {
    // FIXME: If necessary, rebuild Synapse.
        // FIXME: How do we decide that we need to rebuild Synapse?
    // FIXME: Up Synapse.
    // FIXME: If we have a token for an admin user, test it.
    // FIXME: If we have no token or it doesn't work, create an admin user.
    // FIXME: Write down synapse information in a yaml file.
    // FIXME: If the configuration states that we need to run an `up` script, run it.
    unimplemented!()
}

/// Bring things down.
pub fn down(script: &Option<DownScript>, status: Status) -> Result<(), Error> {
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
pub fn run(script: &Option<Script>) -> Result<(), Error> {
    if let Some(ref code) = script {
        code.run()?;
    }
    Ok(())
}
