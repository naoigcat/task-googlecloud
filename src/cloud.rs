use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::InterruptFlag;
use crate::error::AppError;

const SSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SSH_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, Default)]
/// Runs Cloud SDK commands inside the dedicated, host-verified SSH container.
pub struct Cloud {
    interrupt: Option<InterruptFlag>,
}

impl Cloud {
    /// Creates a cloud command runner without signal handling.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a cloud command runner that can terminate child processes when
    /// the workflow receives SIGINT or SIGTERM.
    pub fn with_interrupt(interrupt: InterruptFlag) -> Self {
        Self {
            interrupt: Some(interrupt),
        }
    }

    pub(crate) fn interrupt(&self) -> Option<InterruptFlag> {
        self.interrupt.clone()
    }

    /// Ensures that the remote Cloud SDK has an authenticated account.
    ///
    /// If no account is configured, this starts the interactive login flow in
    /// the container that owns the temporary credentials.
    pub fn login(&self) -> Result<(), AppError> {
        let account = self.run_capture("gcloud config get account")?;
        if account.trim().is_empty() || account.trim() == "(unset)" {
            self.run_interactive("gcloud auth login")?;
        }
        Ok(())
    }

    /// Selects the Google Cloud project used by subsequent SDK commands.
    pub fn set_project(&self, project: &str) -> Result<(), AppError> {
        self.run_script(
            "set -eu\ngcloud config set project \"$1\"\n",
            &[project.to_string()],
        )?;
        Ok(())
    }

    /// Returns a fresh access token from the remote Cloud SDK.
    pub fn access_token(&self) -> Result<String, AppError> {
        Ok(self
            .run_capture("gcloud auth print-access-token")?
            .trim()
            .to_string())
    }

    fn run_capture(&self, remote_command: &str) -> Result<String, AppError> {
        let child = self
            .ssh(remote_command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let output = wait_for_child(
            child,
            self.interrupt.as_ref(),
            SSH_COMMAND_TIMEOUT,
            "Cloud command",
        )?;
        self.output_text("Cloud command", output)
    }

    fn run_interactive(&self, remote_command: &str) -> Result<(), AppError> {
        let output = wait_for_child(
            self.ssh(remote_command).spawn()?,
            self.interrupt.as_ref(),
            SSH_COMMAND_TIMEOUT,
            remote_command,
        )?;
        if output.status.success() {
            return Ok(());
        }
        Err(AppError::Command {
            operation: remote_command.to_string(),
            status: format_status(output.status.code()),
            details: String::new(),
        })
    }

    fn run_script(&self, script: &str, arguments: &[String]) -> Result<String, AppError> {
        // Keep the script on stdin and pass values as positional parameters so
        // project names remain data rather than becoming shell source.
        let remote_command = if arguments.is_empty() {
            "sh -s --".to_string()
        } else {
            format!(
                "sh -s -- {}",
                arguments
                    .iter()
                    .map(|argument| shell_quote(argument))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let child = self
            .ssh(&remote_command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let output = wait_for_child_with_input(
            child,
            self.interrupt.as_ref(),
            SSH_COMMAND_TIMEOUT,
            "Cloud script",
            Some(script.as_bytes().to_vec()),
        )?;
        self.output_text("Cloud script", output)
    }

    #[doc(hidden)]
    pub fn ssh_arguments(&self, remote_command: &str) -> Vec<String> {
        self.ssh(remote_command)
            .get_args()
            .map(|argument| argument.to_string_lossy().to_string())
            .collect()
    }

    fn ssh(&self, remote_command: &str) -> Command {
        let mut command = Command::new("ssh");
        // The container is ephemeral, so use its mounted key and known-hosts
        // files rather than accepting SSH identity data at runtime.
        command.args([
            "-i",
            "/run/googlecloud-ssh/client_key",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectionAttempts=5",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "UserKnownHostsFile=/run/googlecloud-ssh/known_hosts",
            "cloud@googlecloud",
            remote_command,
        ]);
        command
    }

    fn check_output(&self, operation: &str, output: &Output) -> Result<(), AppError> {
        if output.status.success() {
            return Ok(());
        }

        let details = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let details = if details.is_empty() {
            String::new()
        } else {
            format!(": {details}")
        };
        Err(AppError::Command {
            operation: operation.to_string(),
            status: format_status(output.status.code()),
            details,
        })
    }

    fn output_text(&self, operation: &str, output: Output) -> Result<String, AppError> {
        self.check_output(operation, &output)?;
        String::from_utf8(output.stdout).map_err(|error| {
            AppError::Message(format!("{operation} returned invalid UTF-8: {error}"))
        })
    }
}

fn wait_for_child(
    child: Child,
    interrupt: Option<&InterruptFlag>,
    timeout: Duration,
    operation: &str,
) -> Result<Output, AppError> {
    wait_for_child_with_input(child, interrupt, timeout, operation, None)
}

fn wait_for_child_with_input(
    mut child: Child,
    interrupt: Option<&InterruptFlag>,
    timeout: Duration,
    operation: &str,
    input: Option<Vec<u8>>,
) -> Result<Output, AppError> {
    // Drain both output pipes and write stdin on separate threads so a child
    // blocked on a full pipe cannot deadlock the polling loop.
    let pipes = ChildPipes::new(&mut child, input);
    let started = Instant::now();

    // Polling keeps signal and timeout checks responsive while the pipe threads
    // continue draining output; waiting directly on the child would delay both.
    loop {
        if let Some(status) = child.try_wait()? {
            return pipes.finish(status);
        }

        if interrupt.is_some_and(InterruptFlag::is_interrupted) {
            terminate_child(&mut child)?;
            pipes.discard();
            return Err(AppError::Interrupted);
        }

        if started.elapsed() >= timeout {
            terminate_child(&mut child)?;
            pipes.discard();
            return Err(AppError::Message(format!(
                "{operation} timed out after {} seconds",
                timeout.as_secs()
            )));
        }

        thread::sleep(SSH_POLL_INTERVAL);
    }
}

struct ChildPipes {
    stdout: Option<JoinHandle<io::Result<Vec<u8>>>>,
    stderr: Option<JoinHandle<io::Result<Vec<u8>>>>,
    stdin: Option<JoinHandle<io::Result<()>>>,
}

impl ChildPipes {
    fn new(child: &mut Child, input: Option<Vec<u8>>) -> Self {
        Self {
            stdout: spawn_reader(child.stdout.take()),
            stderr: spawn_reader(child.stderr.take()),
            stdin: spawn_writer(child.stdin.take(), input),
        }
    }

    fn finish(self, status: ExitStatus) -> Result<Output, AppError> {
        let Self {
            stdout,
            stderr,
            stdin,
        } = self;
        let writer_result = join_writer(stdin);
        let stdout = join_reader(stdout);
        let stderr = join_reader(stderr);
        writer_result?;
        Ok(Output {
            status,
            stdout: stdout?,
            stderr: stderr?,
        })
    }

    fn discard(self) {
        let Self {
            stdout,
            stderr,
            stdin,
        } = self;
        discard_writer(stdin);
        discard_reader(stdout);
        discard_reader(stderr);
    }
}

fn spawn_reader<R>(reader: Option<R>) -> Option<JoinHandle<io::Result<Vec<u8>>>>
where
    R: Read + Send + 'static,
{
    reader.map(|mut reader| {
        thread::spawn(move || {
            let mut output = Vec::new();
            reader.read_to_end(&mut output).map(|_| output)
        })
    })
}

fn spawn_writer(
    writer: Option<impl Write + Send + 'static>,
    input: Option<Vec<u8>>,
) -> Option<JoinHandle<io::Result<()>>> {
    let (Some(mut writer), Some(input)) = (writer, input) else {
        return None;
    };
    Some(thread::spawn(move || writer.write_all(&input)))
}

fn join_reader(reader: Option<JoinHandle<io::Result<Vec<u8>>>>) -> Result<Vec<u8>, AppError> {
    let Some(reader) = reader else {
        return Ok(Vec::new());
    };
    reader
        .join()
        .map_err(|_| AppError::Message("Cloud command output reader panicked".to_string()))?
        .map_err(AppError::from)
}

fn discard_reader(reader: Option<JoinHandle<io::Result<Vec<u8>>>>) {
    if let Some(reader) = reader {
        let _ = reader.join();
    }
}

fn join_writer(writer: Option<JoinHandle<io::Result<()>>>) -> Result<(), AppError> {
    let Some(writer) = writer else {
        return Ok(());
    };
    writer
        .join()
        .map_err(|_| AppError::Message("Cloud command input writer panicked".to_string()))?
        .map_err(AppError::from)
}

fn discard_writer(writer: Option<JoinHandle<io::Result<()>>>) {
    if let Some(writer) = writer {
        let _ = writer.join();
    }
}

fn terminate_child(child: &mut Child) -> Result<(), AppError> {
    // Reap the process after killing it; otherwise a timed-out or interrupted
    // Cloud SDK command can remain as a zombie while rollback is running.
    if let Err(error) = child.kill()
        && child.try_wait()?.is_none()
    {
        return Err(error.into());
    }
    child.wait()?;
    Ok(())
}

#[doc(hidden)]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn format_status(status: Option<i32>) -> String {
    status.map_or_else(|| "unknown".to_string(), |status| status.to_string())
}

#[cfg(test)]
#[path = "../tests/unit/cloud.rs"]
mod tests;
