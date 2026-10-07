//! Running another program, and following what it writes while it runs.
//!
//! Everything `mix` cannot do itself is done by a process: nix, systemd, the user and group
//! tools, git. A command either answers a question ([`status`]) or does work
//! worth watching, and a failure is named by the command line that produced it.

pub mod output;

use std::sync::Arc;

use mix_core::identity::InvokingUser;
use mix_core::paths::{DEFAULT_PROFILE_BIN, HOME_MANAGER_PROFILE_NAME, nix_profiles_dir};
use mix_core::{ActivityReporter, Result};
use mix_exec::Scope;
use mix_exec::{Command, Drain};

use crate::effect::exec::output::StreamDrain;

/// How much of a streamed process's output is kept to explain a failure with.
const STREAM_TAIL: usize = 64 * 1024;

fn search_path(user: &InvokingUser) -> String {
    let generation = nix_profiles_dir(&user.home).join(HOME_MANAGER_PROFILE_NAME);
    format!(
        "{}/home-path/bin:{DEFAULT_PROFILE_BIN}:/usr/bin:/bin",
        generation.display()
    )
}

pub(crate) fn exec_error(error: mix_exec::Error) -> mix_core::Error {
    match error {
        mix_exec::Error::Spawn { command, source } => mix_core::Error::Exec { command, source },
        mix_exec::Error::Cancelled { command } => mix_core::Error::Cancelled { command },
        mix_exec::Error::Failed { command, detail } => mix_core::Error::Command { command, detail },
    }
}

/// Nix configuration files read besides the system's `nix.conf`: none, so a user's own settings
/// never change what mix builds.
const NIX_USER_CONF_FILES: &str = "/dev/null";

/// A process run as `user` in an environment of its own: `HOME`, `USER`, `PATH` and no user
/// Nix configuration, whatever the process that started mix had set.
pub(crate) fn command_as(user: &InvokingUser, program: &str, args: &[&str]) -> Command {
    Command::new(program)
        .args(args)
        .as_user(user.uid, user.gid)
        .env_clear()
        .env("NIX_USER_CONF_FILES", NIX_USER_CONF_FILES)
        .env("HOME", &user.home)
        .env("USER", &user.name)
        .env("PATH", search_path(user))
}

/// Hands every line to `activity` as it arrives and keeps only what a later error message would
/// be written from.
struct Reported {
    drain: StreamDrain,
    activity: Arc<dyn ActivityReporter>,
}

impl Drain for Reported {
    fn push(&mut self, chunk: &[u8]) {
        self.drain.push(chunk, self.activity.as_ref());
    }

    fn finish(self: Box<Self>) -> Vec<u8> {
        let Self { drain, activity } = *self;
        drain.finish(activity.as_ref())
    }
}

fn reported(activity: Arc<dyn ActivityReporter>) -> Box<dyn Drain> {
    Box::new(Reported {
        drain: StreamDrain::new(STREAM_TAIL),
        activity,
    })
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub async fn run_as(
    user: &InvokingUser,
    program: &str,
    args: &[&str],
    scope: &Scope,
) -> Result<String> {
    run_as_reporting(user, program, args, scope, None).await
}

/// Like [`run_as`], but reports the command's output line by line while it runs.
pub async fn run_as_reporting(
    user: &InvokingUser,
    program: &str,
    args: &[&str],
    scope: &Scope,
    activity: Option<Arc<dyn ActivityReporter>>,
) -> Result<String> {
    let mut command = command_as(user, program, args);
    if let Some(activity) = activity {
        command = command.stderr(reported(activity));
    }
    run(command, scope).await
}

pub(crate) async fn run(command: Command, scope: &Scope) -> Result<String> {
    Ok(stdout_of(&command.run(scope).await.map_err(exec_error)?))
}

pub(crate) async fn status(command: Command, scope: &Scope) -> Result<bool> {
    let output = command.output(scope).await.map_err(exec_error)?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use mix_core::Error;

    use super::*;

    mix_testchild::install!();

    fn program() -> String {
        mix_testchild::program().display().to_string()
    }

    fn child<'s>(steps: impl IntoIterator<Item = &'s str>) -> Command {
        Command::new(program()).args(mix_testchild::args(steps))
    }

    async fn stream(
        mut pipe: impl tokio::io::AsyncRead + Unpin,
        activity: Arc<dyn ActivityReporter>,
    ) -> Vec<u8> {
        use tokio::io::AsyncReadExt as _;
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).await.unwrap();
        let mut drain = reported(activity);
        drain.push(&bytes);
        drain.finish()
    }

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
    async fn run_reports_the_full_command_line_on_failure() {
        let scope = mix_exec::Scope::root();
        match run_as(
            &current_user(),
            &program(),
            &[mix_testchild::FLAG, "exit:1"],
            &scope,
        )
        .await
        {
            Err(Error::Command { command, .. }) => {
                assert_eq!(command, format!("{} --mix-test-child exit:1", program()));
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_preserves_the_source_error_when_the_command_cannot_be_spawned() {
        let scope = mix_exec::Scope::root();
        match run_as(
            &current_user(),
            "mix-test-nonexistent-binary-xyz",
            &["--gid", "30000"],
            &scope,
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
        let scope = mix_exec::Scope::root();
        scope.cancel(mix_exec::Reason::Interrupted);

        match run_as(
            &current_user(),
            &program(),
            &[mix_testchild::FLAG, "sleep"],
            &scope,
        )
        .await
        {
            Err(Error::Cancelled { command }) => {
                assert_eq!(command, format!("{} --mix-test-child sleep", program()));
            }
            other => panic!("expected Cancelled error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_does_not_deadlock_on_output_larger_than_a_pipe_buffer() {
        let scope = mix_exec::Scope::root();

        let output = run_as(
            &current_user(),
            &program(),
            &[mix_testchild::FLAG, "zeros:200000"],
            &scope,
        )
        .await
        .unwrap();

        assert_eq!(output.len(), 200_000);
    }

    #[derive(Default)]
    struct Recorder {
        lines: std::sync::Mutex<Vec<String>>,
        progress: std::sync::Mutex<Vec<mix_nixlog::BuildProgress>>,
        cleared: std::sync::atomic::AtomicBool,
    }

    impl Recorder {
        fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }

        fn last_progress(&self) -> Option<mix_nixlog::BuildProgress> {
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

        fn progress(&self, progress: &mix_nixlog::BuildProgress) {
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
            r#"@nix {"action":"msg","level":3,"msg":"these 5 derivations will be built:"}"#,
            "\n",
            r#"@nix {"action":"start","id":1,"level":3,"text":"","type":104,"fields":[]}"#,
            "\n",
            r#"@nix {"action":"result","id":1,"type":105,"fields":[2,9,1,0]}"#,
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
        assert_eq!(recorder.lines(), ["these 5 derivations will be built:"]);
        assert_eq!(bytes, b"these 5 derivations will be built:\n");
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
    async fn a_command_run_as_a_user_sees_only_the_environment_mix_gives_it() {
        let home = tempfile::tempdir().unwrap();
        let user = InvokingUser {
            uid: nix::unistd::Uid::current().as_raw(),
            gid: nix::unistd::Gid::current().as_raw(),
            name: "mix-user".to_string(),
            home: home.path().to_path_buf(),
        };

        let output = run_as(
            &user,
            &program(),
            &[mix_testchild::FLAG, "env"],
            &mix_exec::Scope::root(),
        )
        .await
        .unwrap();

        let mut names: Vec<&str> = output
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name))
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["HOME", "NIX_USER_CONF_FILES", "PATH", "USER"]);
        assert!(output.contains("NIX_USER_CONF_FILES=/dev/null"));
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
        let scope = mix_exec::Scope::root();
        let recorder = Arc::new(Recorder::default());
        let cmd = child(["eprint:building", "eprint:done", "print:/nix/store/x"])
            .stderr(reported(Arc::clone(&recorder) as Arc<dyn ActivityReporter>));

        let output = cmd.output(&scope).await.unwrap();

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
        let scope = mix_exec::Scope::root();
        let recorder = Arc::new(Recorder::default());
        let cmd = child([
            r#"eprint:@nix {"action":"msg","level":3,"msg":"these 37 paths will be fetched (91.0 MiB download, 300.0 MiB unpacked):"}"#,
            r#"eprint:@nix {"action":"start","id":1,"level":3,"text":"","type":103,"fields":[]}"#,
            r#"eprint:@nix {"action":"result","id":1,"type":105,"fields":[12,40,1,0]}"#,
            r#"eprint:@nix {"action":"start","id":2,"level":3,"text":"","type":101,"fields":[]}"#,
            r#"eprint:@nix {"action":"result","id":2,"type":105,"fields":[50525798,95420416,0,0]}"#,
        ])
            .stderr(reported(Arc::clone(&recorder) as Arc<dyn ActivityReporter>));

        let output = cmd.output(&scope).await.unwrap();

        assert!(output.status.success());
        let progress = recorder.last_progress().expect("counters were reported");
        assert_eq!(
            (progress.downloads_done, progress.downloads_expected),
            (12, 37)
        );
        assert_eq!(
            (progress.bytes_done, progress.bytes_expected),
            (50_525_798, 91 * 1024 * 1024)
        );
        assert_eq!(
            recorder.lines(),
            ["these 37 paths will be fetched (91.0 MiB download, 300.0 MiB unpacked):"]
        );
    }

    #[tokio::test]
    async fn a_streamed_command_keeps_only_the_tail_of_a_flood_of_output() {
        let scope = mix_exec::Scope::root();
        let recorder = Arc::new(Recorder::default());
        let cmd = child(["count-stderr:60000"])
            .stderr(reported(Arc::clone(&recorder) as Arc<dyn ActivityReporter>));

        let output = cmd.output(&scope).await.unwrap();

        assert!(output.status.success());
        assert_eq!(recorder.lines().len(), 60000);
        assert!(output.stderr.len() < STREAM_TAIL * 2);
        assert!(String::from_utf8_lossy(&output.stderr).ends_with("60000\n"));
    }
}
