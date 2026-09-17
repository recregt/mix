use std::path::Path;
use std::sync::Arc;

use mix_core::paths::DEFAULT_PROFILE_BIN;
use mix_core::privilege::InvokingUser;
use mix_core::{ActivityReporter, CancellationToken, Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::Instrument;

use crate::shared::output::{LineSplitter, TailBuffer};

pub(crate) const DIR_MODE_MASK: u32 = 0o7777;

/// Read size for a streamed pipe: large enough that a burst of output costs one syscall,
/// small enough that a single slow line still reaches the screen immediately.
const STREAM_CHUNK: usize = 8 * 1024;

/// How much of a streamed process's output is kept to explain a failure with.
const STREAM_TAIL: usize = 64 * 1024;

fn path_with_nix_profile() -> String {
    match std::env::var("PATH") {
        Ok(path) => format!("{DEFAULT_PROFILE_BIN}:{path}"),
        Err(_) => DEFAULT_PROFILE_BIN.to_string(),
    }
}

fn command_as(user: &InvokingUser, command: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .uid(user.uid)
        .gid(user.gid)
        .env("HOME", &user.home)
        .env("USER", &user.name)
        .env("PATH", path_with_nix_profile());
    cmd
}

/// Drains a pipe, handing every line to `activity` as it arrives and keeping only the tail of
/// the stream for a later error message.
async fn stream(mut pipe: impl AsyncRead + Unpin, activity: Arc<dyn ActivityReporter>) -> Vec<u8> {
    let mut chunk = vec![0u8; STREAM_CHUNK];
    let mut splitter = LineSplitter::new();
    let mut tail = TailBuffer::new(STREAM_TAIL);

    while let Ok(read) = pipe.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        let bytes = &chunk[..read];
        tail.extend(bytes);
        splitter.push(bytes, |line| activity.line(line));
    }

    splitter.finish(|line| activity.line(line));
    activity.clear();
    tail.into_bytes()
}

async fn run_command(
    command: Command,
    command_line: String,
    token: &CancellationToken,
) -> Result<std::process::Output> {
    run_command_reporting(command, command_line, token, None).await
}

async fn run_command_reporting(
    mut command: Command,
    command_line: String,
    token: &CancellationToken,
    activity: Option<Arc<dyn ActivityReporter>>,
) -> Result<std::process::Output> {
    tracing::debug!("running command: {command_line}");

    let mut child = command
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
    // Progress goes to stderr, so that is the pipe worth watching live. The reader keeps the
    // caller's span so the reporter can draw on the line the step already owns.
    let stderr_task = match activity {
        Some(activity) => tokio::spawn(stream(stderr_pipe, activity).in_current_span()),
        None => tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf).await;
            buf
        }),
    };

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

    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

pub async fn run(command: &str, args: &[&str], token: &CancellationToken) -> Result<()> {
    let command_line = format_command(command, args);
    let mut cmd = Command::new(command);
    cmd.args(args);
    let output = run_command(cmd, command_line.clone(), token).await?;
    if !output.status.success() {
        return Err(command_error(command_line, &output));
    }
    Ok(())
}

pub async fn run_as(
    user: &InvokingUser,
    command: &str,
    args: &[&str],
    token: &CancellationToken,
) -> Result<String> {
    run_as_reporting(user, command, args, token, None).await
}

/// Like [`run_as`], but reports the command's output line by line while it runs.
pub async fn run_as_reporting(
    user: &InvokingUser,
    command: &str,
    args: &[&str],
    token: &CancellationToken,
    activity: Option<Arc<dyn ActivityReporter>>,
) -> Result<String> {
    let command_line = format_command(command, args);
    let cmd = command_as(user, command, args);
    let output = run_command_reporting(cmd, command_line.clone(), token, activity).await?;
    if !output.status.success() {
        return Err(command_error(command_line, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn status_as(
    user: &InvokingUser,
    command: &str,
    args: &[&str],
    token: &CancellationToken,
) -> Result<bool> {
    let command_line = format_command(command, args);
    let cmd = command_as(user, command, args);
    let output = run_command(cmd, command_line, token).await?;
    Ok(output.status.success())
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

pub async fn write_file_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("writing file atomically: {}", path.display());

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp_path = dir.join(format!(
        ".{file_name}.mix-tmp-{}-{}",
        std::process::id(),
        next_temp_nonce()
    ));

    let guard = TempFileGuard::new(temp_path.clone());
    write_and_sync(&temp_path, contents.as_ref()).await?;

    tokio::fs::rename(&temp_path, path)
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    guard.disarm();

    sync_dir_best_effort(dir).await;
    Ok(())
}

fn next_temp_nonce() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

struct TempFileGuard {
    path: std::path::PathBuf,
    armed: bool,
}

impl TempFileGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) async fn sync_dir_best_effort(dir: &Path) {
    if let Ok(handle) = tokio::fs::File::open(dir).await {
        let _ = handle.sync_all().await;
    }
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

pub async fn systemd_restart_if_active(name: &str, token: &CancellationToken) -> Result<bool> {
    if !systemd_unit_is_active(name).await {
        return Ok(false);
    }
    run("systemctl", &["restart", name], token).await?;
    Ok(true)
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
    async fn write_file_atomic_temp_names_do_not_collide_under_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        let tasks: Vec<_> = (0..20)
            .map(|i| {
                let path = path.clone();
                tokio::spawn(async move {
                    write_file_atomic(&path, format!("content-{i}").into_bytes()).await
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }

        assert!(path.exists());
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("nix.conf")],
            "no orphaned or colliding temp file should remain"
        );
    }

    #[test]
    fn temp_file_guard_removes_the_file_when_dropped_while_armed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leftover");
        std::fs::write(&path, b"x").unwrap();

        drop(TempFileGuard::new(path.clone()));

        assert!(!path.exists());
    }

    #[test]
    fn temp_file_guard_leaves_the_file_when_disarmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kept");
        std::fs::write(&path, b"x").unwrap();

        TempFileGuard::new(path.clone()).disarm();

        assert!(path.exists());
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

    #[derive(Default)]
    struct Recorder {
        lines: std::sync::Mutex<Vec<String>>,
        cleared: std::sync::atomic::AtomicBool,
    }

    impl Recorder {
        fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }

        fn cleared(&self) -> bool {
            self.cleared.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl ActivityReporter for Recorder {
        fn line(&self, line: &str) {
            self.lines.lock().unwrap().push(line.to_string());
        }

        fn clear(&self) {
            self.cleared
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn stream_reports_each_line_and_returns_the_output() {
        let recorder = Arc::new(Recorder::default());

        let bytes = stream(
            &b"first\nsecond\n"[..],
            Arc::clone(&recorder) as Arc<dyn ActivityReporter>,
        )
        .await;

        assert_eq!(recorder.lines(), ["first", "second"]);
        assert!(recorder.cleared(), "the last line should be cleared");
        assert_eq!(bytes, b"first\nsecond\n");
    }

    #[tokio::test]
    async fn a_streamed_command_reports_its_progress_and_still_returns_its_output() {
        let token = CancellationToken::new();
        let recorder = Arc::new(Recorder::default());
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo building >&2; echo done >&2; echo /nix/store/x"]);

        let output = run_command_reporting(
            cmd,
            "sh".to_string(),
            &token,
            Some(Arc::clone(&recorder) as Arc<dyn ActivityReporter>),
        )
        .await
        .unwrap();

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "/nix/store/x"
        );
        assert_eq!(recorder.lines(), ["building", "done"]);
    }

    #[tokio::test]
    async fn a_streamed_command_keeps_only_the_tail_of_a_flood_of_output() {
        let token = CancellationToken::new();
        let recorder = Arc::new(Recorder::default());
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "seq 1 60000 >&2"]);

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run_command_reporting(
                cmd,
                "sh".to_string(),
                &token,
                Some(Arc::clone(&recorder) as Arc<dyn ActivityReporter>),
            ),
        )
        .await
        .expect("streaming a flood of output should not deadlock")
        .unwrap();

        assert!(output.status.success());
        assert_eq!(recorder.lines().len(), 60000);
        assert!(output.stderr.len() < STREAM_TAIL * 2);
        assert!(String::from_utf8_lossy(&output.stderr).ends_with("60000\n"));
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
