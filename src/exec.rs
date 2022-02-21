use std::{ffi::OsStr, path::PathBuf, process::Stdio};

use anyhow::{anyhow, Context, Error};
use async_trait::async_trait;
use ezexec::lookup::Shell;
use log::info;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Utility class: run a script in a shell.
///
/// Based on ezexec, customized to improve the ability to log.
pub struct Executor {
    /// The shell used to execute the script.
    shell: Shell,
}
impl Executor {
    pub fn try_new() -> Result<Self, Error> {
        let shell = ezexec::lookup::Shell::find()
            .map_err(|e| anyhow!("Could not find a shell to execute command: {}", e))?;
        Ok(Self { shell })
    }

    /// Prepare a `Command` from a script.
    ///
    /// The resulting `Command` will be ready to execute in the shell.
    /// You may customize it with e.g. `env()`.
    pub fn command<P>(&self, cmd: P) -> Result<Command, Error>
    where
        P: AsRef<str>,
    {
        // Lookup shell.
        let shell: &OsStr = self.shell.as_ref();
        let mut command = Command::new(shell);

        // Prefix `command` with the strings we need to call the shell.
        let cmd = cmd.as_ref();
        let execstring_args = self
            .shell
            .execstring_args()
            .map_err(|e| anyhow!("Could not find a shell string: {}", e))?;
        let args = execstring_args.iter().chain(std::iter::once(&cmd));

        command.args(args);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        Ok(command)
    }
}

/// Utility function: spawn an async task to asynchronously write the contents
/// of a reader to both a file and a log.
fn spawn_logger<T>(name: &'static str, reader: BufReader<T>, dest: PathBuf)
where
    BufReader<T>: AsyncBufReadExt + Unpin,
    T: 'static + Send,
{
    tokio::task::spawn(async move {
        let mut file = tokio::fs::File::create(dest)
            .await
            .with_context(|| format!("Could not create log file {}", name))?;
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // Display logs.
            info!("{}: {}", name, line);
            // Store logs to `dest`.
            file.write_all(line.as_bytes())
                .await
                .with_context(|| format!("Could not write log file {}", name))?;
            file.write_all(b"\n")
                .await
                .with_context(|| format!("Could not write log file {}", name))?;
        }
        let _ = file.sync_data().await;
        Ok(()) as Result<(), anyhow::Error>
    });
}

/// Extension trait for `Command`.
#[async_trait]
pub trait CommandExt {
    /// Spawn a command, logging its stdout/stderr to files and to the env logger.
    async fn spawn_logged(&mut self, log_dir: &PathBuf, name: &'static str) -> Result<(), Error>;
}

#[async_trait]
impl CommandExt for Command {
    async fn spawn_logged(&mut self, log_dir: &PathBuf, name: &'static str) -> Result<(), Error> {
        let mut child = self
            .spawn()
            .with_context(|| format!("Could not spawn process for `{}`", name))?;
        // Spawn background tasks to write down stdout.
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            let log_path = log_dir.join(format!("{name}.out", name = name));
            spawn_logger(name, reader, log_path);
        }
        // Spawn background tasks to write down stderr.
        if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            let log_path = log_dir.join(format!("{name}.log", name = name));
            spawn_logger(name, reader, log_path);
        }
        let status = child.wait().await.context("Child process not launched")?;
        if status.success() {
            return Ok(());
        }
        Err(anyhow!("Child `{}` failed: `{}`", name, status))
    }
}
