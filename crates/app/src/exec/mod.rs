//! Running another program, and following what it writes while it runs.
//!
//! Everything `mix` cannot do itself is done by a process: nix, systemd, the user and group
//! tools, git. A command either answers a question — [`status_as`], [`plan_as`] — or does work
//! worth watching, and a failure is named by the command line that produced it.

pub mod output;

use std::fmt::Write as _;
use std::sync::Arc;

use mix_core::nix_plan::PlanError;
use mix_core::paths::DEFAULT_PROFILE_BIN;
use mix_core::privilege::InvokingUser;
use mix_core::{ActivityReporter, BuildPlan, CancellationToken, Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::Instrument;

use crate::exec::output::StreamDrain;

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

/// Drains a pipe, handing every line to `activity` as it arrives and keeping only what a later
/// error message would be written from.
async fn stream(mut pipe: impl AsyncRead + Unpin, activity: Arc<dyn ActivityReporter>) -> Vec<u8> {
    let mut chunk = vec![0u8; STREAM_CHUNK];
    let mut drain = StreamDrain::new(STREAM_TAIL);

    while let Ok(read) = pipe.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        drain.push(&chunk[..read], activity.as_ref());
    }

    drain.finish(activity.as_ref())
}

async fn run_command(
    command: Command,
    command_line: &str,
    token: &CancellationToken,
) -> Result<std::process::Output> {
    run_command_reporting(command, command_line, token, None, None).await
}

/// The command line is borrowed rather than handed over: it is only ever needed as an owned
/// string to name a failure with, and a command that succeeds is the common case.
async fn run_command_reporting(
    mut command: Command,
    command_line: &str,
    token: &CancellationToken,
    activity: Option<Arc<dyn ActivityReporter>>,
    input: Option<Vec<u8>>,
) -> Result<std::process::Output> {
    tracing::debug!("running command: {command_line}");

    if input.is_some() {
        command.stdin(std::process::Stdio::piped());
    }
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Exec {
            command: command_line.to_string(),
            source: e,
        })?;

    let stdin_task = input.map(|input| {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
        })
    });

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
            command: command_line.to_string(),
            source: e,
        })?,
        () = token.cancelled() => {
            child.kill().await.map_err(|e| Error::Exec {
                command: command_line.to_string(),
                source: e,
            })?;
            stdout_task.abort();
            stderr_task.abort();
            if let Some(stdin_task) = &stdin_task {
                stdin_task.abort();
            }
            return Err(Error::Cancelled { command: command_line.to_string() });
        }
    };

    if let Some(stdin_task) = stdin_task {
        let _ = stdin_task.await;
    }
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
    let output = run_command(cmd, &command_line, token).await?;
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
    let output = run_command_reporting(cmd, &command_line, token, activity, None).await?;
    if !output.status.success() {
        return Err(command_error(command_line, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn run_as_with_input(
    user: &InvokingUser,
    command: &str,
    args: &[&str],
    input: Vec<u8>,
    token: &CancellationToken,
) -> Result<String> {
    let command_line = format_command(command, args);
    let cmd = command_as(user, command, args);
    let output = run_command_reporting(cmd, &command_line, token, None, Some(input)).await?;
    if !output.status.success() {
        return Err(command_error(command_line, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Runs a nix dry run as `user` and reads back the plan it printed.
///
/// A dry run answers a question instead of doing work: it prints its plan and exits, so its
/// output is read once it is complete rather than streamed, and it is left in plain text so a
/// failure still reads as prose.
pub async fn plan_as(
    user: &InvokingUser,
    command: &str,
    args: &[&str],
    token: &CancellationToken,
) -> Result<std::result::Result<BuildPlan, PlanError>> {
    let command_line = format_command(command, args);
    let cmd = command_as(user, command, args);
    let output = run_command(cmd, &command_line, token).await?;
    if !output.status.success() {
        return Err(command_error(command_line, &output));
    }
    Ok(BuildPlan::parse(&String::from_utf8_lossy(&output.stderr)))
}

pub async fn status_as(
    user: &InvokingUser,
    command: &str,
    args: &[&str],
    token: &CancellationToken,
) -> Result<bool> {
    let command_line = format_command(command, args);
    let cmd = command_as(user, command, args);
    let output = run_command(cmd, &command_line, token).await?;
    Ok(output.status.success())
}

/// Renders a command line for a log line or an error message.
///
/// Built in one buffer sized for the whole line: this runs for every command the tool spawns,
/// including the ones that only answer a question.
pub fn format_command(command: &str, args: &[&str]) -> String {
    let width = command.len() + args.iter().map(|arg| arg.len() + 3).sum::<usize>();
    let mut rendered = String::with_capacity(width);
    rendered.push_str(command);

    for arg in args {
        rendered.push(' ');
        if arg.is_empty() || arg.contains(char::is_whitespace) {
            // An argument that would not survive being read back as one word is quoted.
            let _ = write!(rendered, "{arg:?}");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The user the test itself runs as: switching to it is a no-op, so a command can be run
    /// without any privilege at all.
    fn current_user() -> InvokingUser {
        InvokingUser {
            uid: nix::unistd::Uid::current().as_raw(),
            gid: nix::unistd::Gid::current().as_raw(),
            name: "mix-test".to_string(),
            home: std::env::temp_dir(),
        }
    }

    #[tokio::test]
    async fn plan_as_reads_back_the_derivations_a_dry_run_would_build() {
        let plan = plan_as(
            &current_user(),
            "/bin/sh",
            &[
                "-c",
                "printf 'this derivation will be built:\\n  \
                 /nix/store/00000000000000000000000000000001-hello.drv\\n' >&2",
            ],
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(
            plan.to_build(),
            ["/nix/store/00000000000000000000000000000001-hello.drv".to_string()]
        );
    }

    #[tokio::test]
    async fn plan_as_hands_back_a_plan_it_could_not_read_rather_than_an_empty_one() {
        let plan = plan_as(
            &current_user(),
            "/bin/sh",
            &[
                "-c",
                "printf 'these 1 derivations are going to be built:\\n  \
                 /nix/store/00000000000000000000000000000001-hello.drv\\n' >&2",
            ],
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(matches!(plan, Err(PlanError::Unannounced { .. })));
    }

    #[tokio::test]
    async fn input_larger_than_a_pipe_buffer_is_written_while_the_output_is_read() {
        let input = vec![b'x'; 1024 * 1024];

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run_as_with_input(
                &current_user(),
                "/bin/cat",
                &[],
                input.clone(),
                &CancellationToken::new(),
            ),
        )
        .await
        .expect("writing stdin must not wait for the output to be read")
        .unwrap();

        assert_eq!(output.len(), input.len());
    }

    #[tokio::test]
    async fn a_command_that_ignores_its_input_still_finishes() {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run_as_with_input(
                &current_user(),
                "/bin/sh",
                &["-c", "echo done"],
                vec![b'x'; 1024 * 1024],
                &CancellationToken::new(),
            ),
        )
        .await
        .expect("a reader that never reads must not hang the writer")
        .unwrap();

        assert_eq!(output, "done");
    }

    #[tokio::test]
    async fn plan_as_reports_a_dry_run_that_failed_rather_than_an_empty_plan() {
        let err = plan_as(
            &current_user(),
            "/bin/sh",
            &["-c", "echo \"error: attribute 'nope' missing\" >&2; exit 1"],
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("attribute 'nope' missing"));
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
        progress: std::sync::Mutex<Vec<mix_core::BuildProgress>>,
        cleared: std::sync::atomic::AtomicBool,
    }

    impl Recorder {
        fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }

        fn last_progress(&self) -> Option<mix_core::BuildProgress> {
            self.progress.lock().unwrap().last().copied()
        }

        fn cleared(&self) -> bool {
            self.cleared.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl ActivityReporter for Recorder {
        fn line(&self, line: &str) {
            self.lines.lock().unwrap().push(line.to_string());
        }

        fn progress(&self, progress: &mix_core::BuildProgress) {
            self.progress.lock().unwrap().push(*progress);
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
    async fn stream_reports_structured_records_as_counters() {
        let recorder = Arc::new(Recorder::default());
        let records = concat!(
            r#"@nix {"action":"start","id":1,"level":3,"text":"","type":104,"fields":[]}"#,
            "\n",
            r#"@nix {"action":"result","id":1,"type":105,"fields":[2,5,1,0]}"#,
            "\n",
        );

        let bytes = stream(
            records.as_bytes(),
            Arc::clone(&recorder) as Arc<dyn ActivityReporter>,
        )
        .await;

        let progress = recorder.last_progress().expect("counters were reported");
        assert_eq!(progress.builds_done, 2);
        assert_eq!(progress.builds_expected, 5);
        assert!(recorder.lines().is_empty(), "records are not shown as text");
        // No diagnostic was decoded, and raw records would explain nothing to a reader.
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn stream_keeps_plain_lines_that_arrive_alongside_records() {
        let recorder = Arc::new(Recorder::default());
        let records = concat!(
            r#"@nix {"action":"msg","level":0,"msg":"error: build failed"}"#,
            "\n",
            "warning: from something that is not nix\n",
        );

        let bytes = stream(
            records.as_bytes(),
            Arc::clone(&recorder) as Arc<dyn ActivityReporter>,
        )
        .await;

        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "error: build failed\nwarning: from something that is not nix\n"
        );
    }

    #[tokio::test]
    async fn stream_keeps_the_decoded_diagnostics_of_a_structured_stream() {
        let recorder = Arc::new(Recorder::default());
        let records = concat!(
            r#"@nix {"action":"start","id":1,"level":3,"text":"","type":104,"fields":[]}"#,
            "\n",
            r#"@nix {"action":"msg","level":0,"msg":"error: attribute 'nope' missing"}"#,
            "\n",
        );

        let bytes = stream(
            records.as_bytes(),
            Arc::clone(&recorder) as Arc<dyn ActivityReporter>,
        )
        .await;

        assert_eq!(bytes, b"error: attribute 'nope' missing\n");
        assert_eq!(recorder.lines(), ["error: attribute 'nope' missing"]);
    }

    #[tokio::test]
    async fn stream_leaves_a_plain_text_stream_alone() {
        let recorder = Arc::new(Recorder::default());

        let bytes = stream(
            &b"  indented failure detail  \nsecond line\n"[..],
            Arc::clone(&recorder) as Arc<dyn ActivityReporter>,
        )
        .await;

        // The raw bytes are kept verbatim, indentation and all, for the error message.
        assert_eq!(bytes, b"  indented failure detail  \nsecond line\n");
        assert!(recorder.last_progress().is_none());
    }

    #[tokio::test]
    async fn a_streamed_command_reports_its_progress_and_still_returns_its_output() {
        let token = CancellationToken::new();
        let recorder = Arc::new(Recorder::default());
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo building >&2; echo done >&2; echo /nix/store/x"]);

        let output = run_command_reporting(
            cmd,
            "sh",
            &token,
            Some(Arc::clone(&recorder) as Arc<dyn ActivityReporter>),
            None,
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

    /// The whole path a build takes: a real pipe, split into lines, folded into counters and
    /// handed to the reporter that draws them.
    #[tokio::test]
    async fn a_streamed_command_reports_structured_progress_end_to_end() {
        let token = CancellationToken::new();
        let recorder = Arc::new(Recorder::default());
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            concat!(
                r#"echo '@nix {"action":"start","id":1,"level":3,"text":"","type":103,"fields":[]}' >&2;"#,
                r#"echo '@nix {"action":"result","id":1,"type":105,"fields":[12,37,1,0]}' >&2;"#,
                r#"echo '@nix {"action":"start","id":2,"level":3,"text":"","type":100,"fields":[]}' >&2;"#,
                r#"echo '@nix {"action":"result","id":2,"type":105,"fields":[50525798,95420416,0,0]}' >&2"#,
            ),
        ]);

        let output = run_command_reporting(
            cmd,
            "sh",
            &token,
            Some(Arc::clone(&recorder) as Arc<dyn ActivityReporter>),
            None,
        )
        .await
        .unwrap();

        assert!(output.status.success());
        let progress = recorder.last_progress().expect("counters were reported");
        assert_eq!(
            (progress.downloads_done, progress.downloads_expected),
            (12, 37)
        );
        assert_eq!(
            (progress.bytes_done, progress.bytes_expected),
            (50_525_798, 95_420_416)
        );
        assert!(recorder.lines().is_empty());
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
                "sh",
                &token,
                Some(Arc::clone(&recorder) as Arc<dyn ActivityReporter>),
                None,
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
}
