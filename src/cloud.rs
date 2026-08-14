use std::io::Write;
use std::process::{Command, Output, Stdio};

use crate::error::AppError;

#[derive(Clone, Debug, Default)]
pub struct Cloud;

impl Cloud {
    pub fn new() -> Self {
        Self
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
        let output = self.ssh(remote_command).output()?;
        self.output_text("Cloud command", output)
    }

    fn run_interactive(&self, remote_command: &str) -> Result<(), AppError> {
        let status = self.ssh(remote_command).status()?;
        if status.success() {
            return Ok(());
        }
        Err(AppError::Command {
            operation: remote_command.to_string(),
            status: format_status(status.code()),
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
        let mut child = self
            .ssh(&remote_command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("stdin was configured")
            .write_all(script.as_bytes())?;
        let output = child.wait_with_output()?;
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

#[doc(hidden)]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn format_status(status: Option<i32>) -> String {
    status.map_or_else(|| "unknown".to_string(), |status| status.to_string())
}
