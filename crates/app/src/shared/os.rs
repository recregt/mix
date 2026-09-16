use std::path::Path;

use mix_core::{CancellationToken, Error, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub(crate) const DIR_MODE_MASK: u32 = 0o7777;

pub async fn run(command: &str, args: &[&str], token: &CancellationToken) -> Result<()> {
    let command_line = format_command(command, args);
    tracing::debug!("running command: {command_line}");

    let mut child = Command::new(command)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Exec {
            command: command_line.clone(),
            source: e,
        })?;

    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf).await;
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf).await;
        buf
    });

    let status = tokio::select! {
        status = child.wait() => status.map_err(|e| Error::Exec {
            command: command_line.clone(),
            source: e,
        })?,
        () = token.cancelled() => {
            child.kill().await.map_err(|e| Error::Exec {
                command: command_line.clone(),
                source: e,
            })?;
            stdout_task.abort();
            stderr_task.abort();
            return Err(Error::Cancelled { command: command_line });
        }
    };

    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();

    tracing::trace!(
        "command output: {command_line}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );

    if !status.success() {
        let output = std::process::Output {
            status,
            stdout,
            stderr,
        };
        return Err(command_error(command_line, &output));
    }

    Ok(())
}

pub fn format_command(command: &str, args: &[&str]) -> String {
    let mut rendered = command.to_string();
    for arg in args {
        rendered.push(' ');
        if arg.is_empty() || arg.contains(char::is_whitespace) {
            rendered.push_str(&format!("{arg:?}"));
        } else {
            rendered.push_str(arg);
        }
    }
    rendered
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

pub async fn path_exists(path: impl AsRef<Path>) -> bool {
    tokio::fs::try_exists(path.as_ref()).await.unwrap_or(false)
}

pub async fn systemd_unit_is_active(name: &str) -> bool {
    tracing::debug!("checking systemd unit is-active: {name}");
    Command::new("systemctl")
        .args(["is-active", "--quiet", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

pub async fn files_match(a: &str, b: &str) -> bool {
    let (a, b) = (tokio::fs::read(a).await, tokio::fs::read(b).await);
    matches!((a, b), (Ok(a), Ok(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn path_exists_true_for_a_real_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(path_exists(dir.path()).await);
    }

    #[tokio::test]
    async fn path_exists_false_when_missing() {
        assert!(!path_exists("/does/not/exist/mix-test").await);
    }

    fn output_with(stdout: &str, stderr: &str, exit_code: i32) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(exit_code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn format_command_with_no_args_has_no_trailing_space() {
        assert_eq!(format_command("systemctl", &[]), "systemctl");
    }

    #[test]
    fn format_command_joins_plain_args() {
        assert_eq!(
            format_command("systemctl", &["daemon-reload"]),
            "systemctl daemon-reload"
        );
    }

    #[test]
    fn format_command_quotes_args_with_whitespace() {
        assert_eq!(
            format_command("useradd", &["--comment", "mix build user 1"]),
            r#"useradd --comment "mix build user 1""#
        );
    }

    #[test]
    fn format_command_quotes_empty_args() {
        assert_eq!(
            format_command("nix-env", &["--option", ""]),
            r#"nix-env --option """#
        );
    }

    #[test]
    fn command_error_prefers_stderr() {
        let output = output_with("out", "err", 1);
        match command_error("mycmd", &output) {
            Error::Command { command, detail } => {
                assert_eq!(command, "mycmd");
                assert_eq!(detail, "err");
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[test]
    fn command_error_falls_back_to_stdout_when_stderr_empty() {
        let output = output_with("out-only", "", 1);
        match command_error("mycmd", &output) {
            Error::Command { detail, .. } => assert_eq!(detail, "out-only"),
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[test]
    fn command_error_falls_back_to_status_when_both_empty() {
        let output = output_with("", "", 7);
        match command_error("mycmd", &output) {
            Error::Command { detail, .. } => assert!(detail.contains("exited with status")),
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_reports_the_full_command_line_on_failure() {
        let token = CancellationToken::new();
        match run("false", &["--gid", "30000", "nixbld1"], &token).await {
            Err(Error::Command { command, .. }) => {
                assert_eq!(command, "false --gid 30000 nixbld1");
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_preserves_the_source_error_when_the_command_cannot_be_spawned() {
        let token = CancellationToken::new();
        match run(
            "mix-test-nonexistent-binary-xyz",
            &["--gid", "30000"],
            &token,
        )
        .await
        {
            Err(Error::Exec { command, source }) => {
                assert_eq!(command, "mix-test-nonexistent-binary-xyz --gid 30000");
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Exec error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_kills_and_reaps_the_child_when_cancelled() {
        let token = CancellationToken::new();
        token.cancel();

        match run("sleep", &["5"], &token).await {
            Err(Error::Cancelled { command }) => {
                assert_eq!(command, "sleep 5");
            }
            other => panic!("expected Cancelled error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_does_not_deadlock_on_output_larger_than_a_pipe_buffer() {
        let token = CancellationToken::new();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run("sh", &["-c", "head -c 200000 /dev/zero"], &token),
        )
        .await;

        match result {
            Ok(run_result) => run_result.unwrap(),
            Err(_) => panic!("run() did not return within the timeout, likely deadlocked"),
        }
    }

    #[tokio::test]
    async fn files_match_true_for_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert!(files_match(a.to_str().unwrap(), b.to_str().unwrap()).await);
    }

    #[tokio::test]
    async fn files_match_false_for_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"one").unwrap();
        std::fs::write(&b, b"two").unwrap();
        assert!(!files_match(a.to_str().unwrap(), b.to_str().unwrap()).await);
    }

    #[tokio::test]
    async fn files_match_false_when_one_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"one").unwrap();
        let missing = dir.path().join("missing");
        assert!(!files_match(a.to_str().unwrap(), missing.to_str().unwrap()).await);
    }
}
