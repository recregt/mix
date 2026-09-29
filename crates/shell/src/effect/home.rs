use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use mix_core::Scope;
use mix_core::action::{Action, Expect, Fact, Failure, Outcome, PathFacts, Query};
use mix_core::privilege::InvokingUser;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

use crate::effect::files::{Files, Prepared};

pub const COMMAND: &str = "home-files";

const PROGRAM: &str = "mix home-files";

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Perform(Action),
    Proceed,
    Refuse(Failure),
    Adopt(Vec<PathBuf>),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Prepared(Vec<Action>),
    Done(Outcome),
}

pub fn path_of(action: &Action) -> Option<&Path> {
    match action {
        Action::CreateDir { path, .. }
        | Action::PutFile { path, .. }
        | Action::SetMode { path, .. }
        | Action::SetOwner { path, .. }
        | Action::SetAside { path, .. }
        | Action::RemoveCreated { path, .. }
        | Action::RemoveCreatedTree { path, .. }
        | Action::Restore { path, .. }
        | Action::ReclaimTree { path, .. } => Some(path),
        _ => None,
    }
}

fn broken(detail: impl std::fmt::Display) -> Failure {
    Failure::CommandFailed {
        program: PROGRAM.to_string(),
        status: None,
        output_tail: detail.to_string(),
    }
}

fn send(output: &mut impl Write, message: &impl Serialize) -> Result<(), Failure> {
    serde_json::to_writer(&mut *output, message).map_err(broken)?;
    output.write_all(b"\n").map_err(broken)?;
    output.flush().map_err(broken)
}

fn receive<M: for<'de> Deserialize<'de>>(input: &mut impl BufRead) -> Result<Option<M>, Failure> {
    let mut line = String::new();
    if input.read_line(&mut line).map_err(broken)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line).map(Some).map_err(broken)
}

pub fn serve(
    request: &str,
    mut input: impl BufRead + Send,
    mut output: impl Write + Send,
) -> Result<(), Failure> {
    let uid = nix::unistd::getuid().as_raw();
    let mut files =
        Files::open_trusting(Path::new("/"), request, uid).map_err(|error| Failure::Io {
            path: "/".into(),
            kind: error.kind(),
        })?;
    while let Some(request) = receive::<Request>(&mut input)? {
        let action = match request {
            Request::Adopt(pending) => {
                files.adopt(pending);
                continue;
            }
            Request::Perform(action) => action,
            other => return Err(broken(format!("{other:?} before an action"))),
        };
        let outcome = {
            let mut prepared = |undo: &[Action]| {
                send(&mut output, &Reply::Prepared(undo.to_vec()))?;
                match receive::<Request>(&mut input)? {
                    Some(Request::Proceed) => Ok(()),
                    Some(Request::Refuse(failure)) => Err(failure),
                    other => Err(broken(format!("{other:?} in place of an answer"))),
                }
            };
            files
                .perform(&action, &mut prepared)
                .unwrap_or_else(|| Err(broken(format!("{action:?} is not a file action"))))
        };
        send(&mut output, &Reply::Done(outcome))?;
    }
    Ok(())
}

pub struct Agent {
    session: Option<mix_exec::Session>,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl Agent {
    pub fn spawn(user: &InvokingUser, request: &str, scope: &Scope) -> Result<Self, Failure> {
        let program = std::env::current_exe().map_err(|error| Failure::SpawnFailed {
            program: PROGRAM.to_string(),
            kind: error.kind(),
        })?;
        let (session, stdin, stdout) = mix_exec::Command::new(program)
            .args([COMMAND, request])
            .env_clear()
            .env("HOME", &user.home)
            .env("LC_ALL", "C.UTF-8")
            .as_user(user.uid, user.gid)
            .session(scope)
            .map_err(|error| Failure::SpawnFailed {
                program: PROGRAM.to_string(),
                kind: match error {
                    mix_exec::Error::Spawn { source, .. } => source.kind(),
                    _ => std::io::ErrorKind::Other,
                },
            })?;
        Ok(Self {
            session: Some(session),
            stdin,
            stdout: BufReader::new(stdout).lines(),
        })
    }

    async fn send(&mut self, request: &Request) -> Result<(), Failure> {
        let mut line = serde_json::to_vec(request).map_err(broken)?;
        line.push(b'\n');
        match self.stdin.write_all(&line).await {
            Ok(()) => Ok(()),
            Err(_) => Err(self.gone().await),
        }
    }

    async fn receive(&mut self) -> Result<Reply, Failure> {
        match self.stdout.next_line().await {
            Ok(Some(line)) => serde_json::from_str(&line).map_err(broken),
            _ => Err(self.gone().await),
        }
    }

    async fn gone(&mut self) -> Failure {
        let Some(session) = self.session.take() else {
            return broken("the agent had already stopped");
        };
        match session.finish().await {
            Ok(output) => Failure::CommandFailed {
                program: PROGRAM.to_string(),
                status: output.status.code(),
                output_tail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            },
            Err(error) => broken(error),
        }
    }

    pub async fn perform(&mut self, action: &Action, prepared: &mut Prepared<'_>) -> Outcome {
        self.send(&Request::Perform(action.clone())).await?;
        loop {
            match self.receive().await? {
                Reply::Prepared(undo) => {
                    let answer = match prepared(&undo) {
                        Ok(()) => Request::Proceed,
                        Err(failure) => Request::Refuse(failure),
                    };
                    self.send(&answer).await?;
                }
                Reply::Done(outcome) => return outcome,
            }
        }
    }

    pub async fn adopt(&mut self, pending: Vec<PathBuf>) -> Result<(), Failure> {
        self.send(&Request::Adopt(pending)).await
    }

    pub async fn close(mut self) -> Result<(), Failure> {
        drop(self.stdin);
        match self.session.take() {
            Some(session) => match session.finish().await {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => Err(Failure::CommandFailed {
                    program: PROGRAM.to_string(),
                    status: output.status.code(),
                    output_tail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                }),
                Err(error) => Err(broken(error)),
            },
            None => Ok(()),
        }
    }
}

pub fn core_error(failure: Failure, path: &Path) -> mix_core::Error {
    match failure {
        Failure::Io { path, kind } => mix_core::Error::Io {
            path,
            source: kind.into(),
        },
        Failure::SpawnFailed { program, kind } => mix_core::Error::Exec {
            command: program,
            source: kind.into(),
        },
        Failure::Cancelled => mix_core::Error::Cancelled {
            command: format!("change {}", path.display()),
        },
        other => mix_core::Error::Command {
            command: format!("change {}", path.display()),
            detail: format!("{other:?}"),
        },
    }
}

pub fn owner(files: &Files, path: &Path) -> Option<u32> {
    files
        .tree_owner(path)
        .filter(|uid| *uid != nix::unistd::geteuid().as_raw())
}

pub fn account(uid: u32, path: &Path) -> Result<InvokingUser, Failure> {
    let found = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .ok_or_else(|| Failure::Conflict {
            subject: path.display().to_string(),
            expected: "a tree owned by root or by a known user".to_string(),
            found: format!("a tree owned by uid {uid}, who has no account"),
        })?;
    Ok(InvokingUser {
        uid,
        gid: found.gid.as_raw(),
        name: found.name,
        home: found.dir,
    })
}

pub async fn write_file(
    path: &Path,
    contents: &[u8],
    mode: u32,
    scope: &Scope,
) -> Result<(), Failure> {
    let request = format!("write-{}", std::process::id());
    let mut files = Files::open(Path::new("/"), &request).map_err(|error| Failure::Io {
        path: "/".into(),
        kind: error.kind(),
    })?;
    let expect = match files.observe(&Query::Path(path.to_path_buf())) {
        Some(Fact::Path(PathFacts { id: Some(id), .. })) => Expect::Present(id),
        _ => Expect::Absent,
    };
    let put = Action::PutFile {
        path: path.to_path_buf(),
        contents: contents.into(),
        mode,
        owner: None,
        expect,
    };
    let mut prepared = |_: &[Action]| Ok(());
    match owner(&files, path) {
        Some(uid) => {
            let mut agent = Agent::spawn(&account(uid, path)?, &request, scope)?;
            agent.perform(&put, &mut prepared).await?;
            agent.perform(&Action::Commit, &mut prepared).await?;
            agent.close().await
        }
        None => {
            files.perform(&put, &mut prepared).expect("a file action")?;
            files
                .perform(&Action::Commit, &mut prepared)
                .expect("a file action")
                .map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests;
