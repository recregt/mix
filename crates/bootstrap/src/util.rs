use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use mix_core::{Error, Result};
use tokio::process::Command;

pub async fn run(command: &str, args: &[&str]) -> Result<()> {
    let command_line = format_command(command, args);
    tracing::debug!("running command: {command_line}");

    let output = Command::new(command)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::Exec {
            command: command_line.clone(),
            source: e,
        })?;

    tracing::trace!(
        "command output: {command_line}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    if !output.status.success() {
        return Err(command_error(command_line, &output));
    }

    Ok(())
}

pub async fn create_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("creating directory: {}", path.display());
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })
}

pub async fn write_file(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("writing file: {}", path.display());
    tokio::fs::write(path, contents)
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })
}

pub async fn set_permissions(path: impl AsRef<Path>, mode: u32) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("setting permissions: {} ({mode:o})", path.display());
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })
}

pub async fn remove_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("removing directory: {}", path.display());
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

pub async fn remove_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("removing file: {}", path.display());
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

pub async fn copy_file(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();
    tracing::debug!("copying file: {} -> {}", src.display(), dest.display());
    tokio::fs::copy(src, dest)
        .await
        .map(|_| ())
        .map_err(|e| Error::Io {
            path: dest.to_path_buf(),
            source: e,
        })
}

pub(crate) fn format_command(command: &str, args: &[&str]) -> String {
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

pub(crate) fn warn_on_failure<T, E: std::fmt::Display>(
    action: &'static str,
    result: std::result::Result<T, E>,
) {
    if let Err(error) = result {
        tracing::warn!("{action} failed: {error}, continuing");
    }
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
        match run("false", &["--gid", "30000", "nixbld1"]).await {
            Err(Error::Command { command, .. }) => {
                assert_eq!(command, "false --gid 30000 nixbld1");
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_preserves_the_source_error_when_the_command_cannot_be_spawned() {
        match run("mix-test-nonexistent-binary-xyz", &["--gid", "30000"]).await {
            Err(Error::Exec { command, source }) => {
                assert_eq!(command, "mix-test-nonexistent-binary-xyz --gid 30000");
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Exec error, got {other:?}"),
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
