use std::io::{self, Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::InterruptFlag;
use crate::error::AppError;

const SSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SSH_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, Default)]
pub struct Cloud {
    interrupt: Option<InterruptFlag>,
}

impl Cloud {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interrupt(interrupt: InterruptFlag) -> Self {
        Self {
            interrupt: Some(interrupt),
        }
    }

    pub(crate) fn interrupt(&self) -> Option<InterruptFlag> {
        self.interrupt.clone()
    }

    pub fn login(&self) -> Result<(), AppError> {
        let account = self.run_capture("gcloud config get account")?;
        if account.trim().is_empty() || account.trim() == "(unset)" {
            self.run_interactive("gcloud auth login")?;
        }
        Ok(())
    }

    pub fn set_project(&self, project: &str) -> Result<(), AppError> {
        self.run_script(
            "set -eu\ngcloud config set project \"$1\"\n",
            &[project.to_string()],
        )?;
        Ok(())
    }

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
    let mut stdout = spawn_reader(child.stdout.take());
    let mut stderr = spawn_reader(child.stderr.take());
    let mut stdin_writer = spawn_writer(child.stdin.take(), input);
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            let writer_result = join_writer(stdin_writer.take());
            let stdout = join_reader(stdout.take());
            let stderr = join_reader(stderr.take());
            writer_result?;
            return Ok(Output {
                status,
                stdout: stdout?,
                stderr: stderr?,
            });
        }

        if interrupt.is_some_and(InterruptFlag::is_interrupted) {
            terminate_child(&mut child)?;
            discard_writer(stdin_writer.take());
            discard_reader(stdout.take());
            discard_reader(stderr.take());
            return Err(AppError::Interrupted);
        }

        if started.elapsed() >= timeout {
            terminate_child(&mut child)?;
            discard_writer(stdin_writer.take());
            discard_reader(stdout.take());
            discard_reader(stderr.take());
            return Err(AppError::Message(format!(
                "{operation} timed out after {} seconds",
                timeout.as_secs()
            )));
        }

        thread::sleep(SSH_POLL_INTERVAL);
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
mod tests {
    use std::process::Stdio;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};

    use super::{wait_for_child, wait_for_child_with_input};
    use crate::{AppError, InterruptFlag};

    #[test]
    fn terminates_a_child_when_interrupted() {
        let interrupted = Arc::new(AtomicBool::new(false));
        let interrupt = InterruptFlag::from_atomic(Arc::clone(&interrupted));
        let child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        interrupted.store(true, Ordering::Relaxed);
        let started = Instant::now();

        let error = wait_for_child(
            child,
            Some(&interrupt),
            Duration::from_secs(5),
            "test command",
        )
        .unwrap_err();

        assert!(matches!(error, AppError::Interrupted));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn terminates_a_child_when_the_command_times_out() {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let error =
            wait_for_child(child, None, Duration::from_millis(50), "test command").unwrap_err();

        assert!(matches!(error, AppError::Message(message) if message.contains("timed out")));
    }

    #[test]
    fn interrupts_a_child_while_stdin_is_blocked() {
        let interrupted = Arc::new(AtomicBool::new(true));
        let interrupt = InterruptFlag::from_atomic(interrupted);
        let child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();

        let error = wait_for_child_with_input(
            child,
            Some(&interrupt),
            Duration::from_secs(5),
            "test command",
            Some(vec![0; 8 * 1024 * 1024]),
        )
        .unwrap_err();

        assert!(matches!(error, AppError::Interrupted));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
