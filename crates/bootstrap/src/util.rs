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
        return Err(Error::Command {
            command: command.into(),
            detail: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(())
}

pub async fn files_match(a: &str, b: &str) -> bool {
    let (a, b) = (tokio::fs::read(a).await, tokio::fs::read(b).await);
    matches!((a, b), (Ok(a), Ok(b)) if a == b)
}
