use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::os::fd::OwnedFd;
use std::process::{ExitStatus, Output, Stdio};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::group;
use crate::{Error, Scope, Stop};

/// Read size for a streamed pipe: large enough that a burst of output costs one syscall,
/// small enough that a single slow line still reaches the screen immediately.
const STREAM_CHUNK: usize = 8 * 1024;

pub trait Drain: Send + 'static {
    fn push(&mut self, chunk: &[u8]);
    fn finish(self: Box<Self>) -> Vec<u8>;
}

impl Drain for Vec<u8> {
    fn push(&mut self, chunk: &[u8]) {
        self.extend_from_slice(chunk);
    }

    fn finish(self: Box<Self>) -> Vec<u8> {
        *self
    }
}

pub struct Command {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    env_remove: Vec<OsString>,
    env_clear: bool,
    user: Option<(u32, u32)>,
    input: Option<Vec<u8>>,
    stderr: Option<Box<dyn Drain>>,
}

pub struct Foreground(tokio::process::Child);

pub struct Session {
    child: tokio::process::Child,
    _group: group::Group,
    stderr: tokio::task::JoinHandle<Vec<u8>>,
    line: String,
}

impl Session {
    pub async fn finish(mut self) -> Result<Output, Error> {
        let status = self
            .child
            .wait()
            .await
            .map_err(|source| Error::spawn(self.line.clone(), source))?;
        let stderr = self.stderr.await.unwrap_or_default();
        Ok(Output {
            status,
            stdout: Vec::new(),
            stderr,
        })
    }
}

impl Foreground {
    pub async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.0.wait().await
    }
}

impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            env_clear: false,
            user: None,
            input: None,
            stderr: None,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.env_remove.push(key.as_ref().to_os_string());
        self
    }

    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
        self
    }

    pub fn as_user(mut self, uid: u32, gid: u32) -> Self {
        self.user = Some((uid, gid));
        self
    }

    pub fn input(mut self, input: Vec<u8>) -> Self {
        self.input = Some(input);
        self
    }

    pub fn stderr(mut self, drain: Box<dyn Drain>) -> Self {
        self.stderr = Some(drain);
        self
    }

    pub fn line(&self) -> String {
        let args: Vec<_> = self.args.iter().map(|arg| arg.to_string_lossy()).collect();
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        format_command(&self.program.to_string_lossy(), &args)
    }

    #[allow(clippy::disallowed_methods)]
    pub(crate) fn process(&self) -> tokio::process::Command {
        let mut process = tokio::process::Command::new(&self.program);
        process.args(&self.args);
        if self.env_clear {
            process.env_clear();
        }
        for key in &self.env_remove {
            process.env_remove(key);
        }
        process.envs(self.env.iter().map(|(key, value)| (key, value)));
        if let Some((uid, gid)) = self.user {
            process.uid(uid).gid(gid);
        }
        process
    }

    pub fn spawn_foreground(self, stdin: OwnedFd) -> Result<Foreground, Error> {
        let line = self.line();
        self.process()
            .stdin(Stdio::from(stdin))
            .spawn()
            .map(Foreground)
            .map_err(|source| Error::spawn(line, source))
    }

    pub fn session(
        self,
        scope: &Scope,
    ) -> Result<
        (
            Session,
            tokio::process::ChildStdin,
            tokio::process::ChildStdout,
        ),
        Error,
    > {
        let line = self.line();
        scope.started(&line);
        let mut process = self.process();
        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, group) = group::spawn(&mut process, scope.processes())
            .map_err(|source| Error::spawn(line.clone(), source))?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let stderr = tokio::spawn(drain(stderr, Box::new(Vec::new())));
        Ok((
            Session {
                child,
                _group: group,
                stderr,
                line,
            },
            stdin,
            stdout,
        ))
    }

    pub async fn run(self, scope: &Scope) -> Result<Output, Error> {
        let line = self.line();
        let output = self.output(scope).await?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(failure(line, &output))
        }
    }

    pub fn run_blocking(self, scope: &Scope) -> Result<Output, Error> {
        blocking(self.run(scope))
    }

    pub fn output_blocking(self, scope: &Scope) -> Result<Output, Error> {
        blocking(self.output(scope))
    }

    pub async fn output(self, scope: &Scope) -> Result<Output, Error> {
        let line = self.line();
        scope.started(&line);

        let mut process = self.process();
        process
            .stdin(if self.input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, group) = group::spawn(&mut process, scope.processes())
            .map_err(|source| Error::spawn(line.clone(), source))?;

        let stdin_task = self.input.map(|input| {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            tokio::spawn(async move {
                let _ = stdin.write_all(&input).await;
            })
        });

        let stdout_pipe = child.stdout.take().expect("stdout was piped");
        let stderr_pipe = child.stderr.take().expect("stderr was piped");
        let stdout_task = tokio::spawn(drain(stdout_pipe, Box::new(Vec::new())));
        let stderr_task = tokio::spawn(drain(
            stderr_pipe,
            self.stderr.unwrap_or_else(|| Box::new(Vec::new())),
        ));

        let status = tokio::select! {
            status = child.wait() => status.map_err(|source| Error::spawn(line.clone(), source))?,
            stop = scope.stopped() => {
                group.stop(&mut child).await;
                stdout_task.abort();
                stderr_task.abort();
                if let Some(stdin_task) = &stdin_task {
                    stdin_task.abort();
                }
                return Err(Error::stopped(line, stop));
            }
        };

        scope.finished(&line, status);
        if let Some(stdin_task) = stdin_task {
            let _ = stdin_task.await;
        }
        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();

        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

fn blocking(work: impl Future<Output = Result<Output, Error>>) -> Result<Output, Error> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(work),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|source| Error::spawn("a runtime to run it on".to_string(), source))?
            .block_on(work),
    }
}

impl Error {
    fn spawn(command: String, source: std::io::Error) -> Self {
        Self::Spawn { command, source }
    }

    fn stopped(command: String, stop: Stop) -> Self {
        match stop {
            Stop::Cancelled => Self::Cancelled { command },
        }
    }
}

async fn drain(mut pipe: impl AsyncRead + Unpin, mut drain: Box<dyn Drain>) -> Vec<u8> {
    let mut chunk = vec![0u8; STREAM_CHUNK];
    while let Ok(read) = pipe.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        drain.push(&chunk[..read]);
    }
    drain.finish()
}

/// Renders a command line for a log line or an error message.
///
/// Built in one buffer sized for the whole line: this runs for every command the tool spawns,
/// including the ones that only answer a question.
fn format_command(command: &str, args: &[&str]) -> String {
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

fn failure(command: String, output: &Output) -> Error {
    let detail = if !output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stderr).trim().to_string()
    } else if !output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        format!("exited with status {}", output.status)
    };
    Error::Failed { command, detail }
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
    fn a_failure_prefers_stderr() {
        let output = output_with("out", "err", 1);
        match failure("mycmd".to_string(), &output) {
            Error::Failed { command, detail } => {
                assert_eq!(command, "mycmd");
                assert_eq!(detail, "err");
            }
            other => panic!("expected a Failed error, got {other:?}"),
        }
    }

    #[test]
    fn a_failure_falls_back_to_stdout_when_stderr_empty() {
        let output = output_with("out-only", "", 1);
        match failure("mycmd".to_string(), &output) {
            Error::Failed { detail, .. } => assert_eq!(detail, "out-only"),
            other => panic!("expected a Failed error, got {other:?}"),
        }
    }

    #[test]
    fn a_failure_falls_back_to_status_when_both_empty() {
        let output = output_with("", "", 7);
        match failure("mycmd".to_string(), &output) {
            Error::Failed { detail, .. } => assert!(detail.contains("exited with status")),
            other => panic!("expected a Failed error, got {other:?}"),
        }
    }
}
