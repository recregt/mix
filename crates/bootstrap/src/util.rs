use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use mix_core::{CancellationToken, Error, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

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
            return Err(Error::Cancelled { command: command_line });
        }
    };

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout).await;
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr).await;
    }

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

pub async fn create_dir_with_mode(path: impl AsRef<Path>, mode: u32) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!(
        "creating directory with mode: {} ({mode:o})",
        path.display()
    );

    let previous_umask = nix::sys::stat::umask(nix::sys::stat::Mode::empty());
    let result = tokio::fs::DirBuilder::new().mode(mode).create(path).await;
    nix::sys::stat::umask(previous_umask);

    result.map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })
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

pub async fn write_file_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("writing file atomically: {}", path.display());

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp_path = dir.join(format!(".{file_name}.mix-tmp-{}", std::process::id()));

    if let Err(e) = write_and_sync(&temp_path, contents.as_ref()).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(e);
    }

    tokio::fs::rename(&temp_path, path)
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })
}

pub async fn copy_file_atomic(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();
    tracing::debug!(
        "copying file atomically: {} -> {}",
        src.display(),
        dest.display()
    );
    let contents = tokio::fs::read(src).await.map_err(|e| Error::Io {
        path: src.to_path_buf(),
        source: e,
    })?;
    write_file_atomic(dest, contents).await
}

async fn write_and_sync(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = tokio::fs::File::create(path).await.map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    file.write_all(contents).await.map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    file.sync_all().await.map_err(|e| Error::Io {
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

pub async fn path_exists(path: impl AsRef<Path>) -> bool {
    tokio::fs::try_exists(path.as_ref()).await.unwrap_or(false)
}

pub async fn is_file(path: impl AsRef<Path>) -> bool {
    tokio::fs::metadata(path.as_ref())
        .await
        .is_ok_and(|meta| meta.is_file())
}

pub async fn is_dir(path: impl AsRef<Path>) -> bool {
    tokio::fs::metadata(path.as_ref())
        .await
        .is_ok_and(|meta| meta.is_dir())
}

pub const DIR_MODE_MASK: u32 = 0o7777;

pub async fn dir_has_mode(path: impl AsRef<Path>, mode: u32) -> bool {
    match tokio::fs::metadata(path.as_ref()).await {
        Ok(meta) => meta.is_dir() && meta.permissions().mode() & DIR_MODE_MASK == mode,
        Err(_) => false,
    }
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

    #[tokio::test]
    async fn path_exists_true_for_a_real_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(path_exists(dir.path()).await);
    }

    #[tokio::test]
    async fn path_exists_false_when_missing() {
        assert!(!path_exists("/does/not/exist/mix-test").await);
    }

    #[tokio::test]
    async fn is_file_true_for_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(is_file(&file).await);
    }

    #[tokio::test]
    async fn is_file_false_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_file(dir.path()).await);
    }

    #[tokio::test]
    async fn is_file_false_when_missing() {
        assert!(!is_file("/does/not/exist/mix-test").await);
    }

    #[tokio::test]
    async fn is_dir_true_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_dir(dir.path()).await);
    }

    #[tokio::test]
    async fn is_dir_false_for_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(!is_dir(&file).await);
    }

    #[tokio::test]
    async fn is_dir_false_when_missing() {
        assert!(!is_dir("/does/not/exist/mix-test").await);
    }

    #[tokio::test]
    async fn create_dir_with_mode_ignores_the_process_umask() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sticky");

        let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
        let result = create_dir_with_mode(&target, 0o1777).await;
        nix::sys::stat::umask(previous);

        result.unwrap();
        assert!(dir_has_mode(&target, 0o1777).await);
    }

    #[tokio::test]
    async fn dir_has_mode_true_when_mode_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(dir_has_mode(dir.path(), 0o755).await);
    }

    #[tokio::test]
    async fn dir_has_mode_true_for_a_sticky_world_writable_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(dir_has_mode(dir.path(), 0o1777).await);
    }

    #[tokio::test]
    async fn dir_has_mode_false_on_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!dir_has_mode(dir.path(), 0o755).await);
    }

    #[tokio::test]
    async fn dir_has_mode_false_when_missing() {
        assert!(!dir_has_mode(Path::new("/does/not/exist/mix-test"), 0o755).await);
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
    async fn write_file_atomic_writes_the_full_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        write_file_atomic(&path, b"hello").await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn write_file_atomic_replaces_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");
        std::fs::write(&path, b"old").unwrap();

        write_file_atomic(&path, b"new").await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[tokio::test]
    async fn write_file_atomic_leaves_no_temp_file_behind_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        write_file_atomic(&path, b"hello").await.unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("nix.conf")]);
    }

    #[tokio::test]
    async fn write_file_atomic_does_not_touch_the_destination_when_the_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-subdir").join("nix.conf");

        assert!(write_file_atomic(&path, b"hello").await.is_err());

        assert!(!path.exists());
    }

    #[tokio::test]
    async fn copy_file_atomic_copies_content_to_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::write(&src, b"unit-file-content").unwrap();

        copy_file_atomic(&src, &dest).await.unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"unit-file-content");
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
