use mix_core::{Error, Result};
use tokio::process::Command;

pub async fn run(command: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(command)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::Command {
            command: command.into(),
            detail: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(command_error(command, &output));
    }

    Ok(())
}

pub fn command_error(command: impl Into<String>, output: &std::process::Output) -> Error {
    let detail = if !output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stderr).trim().to_string()
    } else if !output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        format!("exited with status {}", output.status)
    };
    Error::Command {
        command: command.into(),
        detail,
    }
}

pub async fn files_match(a: &str, b: &str) -> bool {
    let (a, b) = (tokio::fs::read(a).await, tokio::fs::read(b).await);
    matches!((a, b), (Ok(a), Ok(b)) if a == b)
}
