use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::RUNNING_PROGRAM;
use mix_core::effect::{Action, Fact, Failure, Outcome, Query};
use mix_exec::Scope;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

use mix_fs::{Files, Prepared};

mod generation;
mod lock;

pub use generation::{
    Active, active, active_generation, active_input, built, built_generation, current, existing,
    generation_of, profile_link, store_generation, store_input,
};
pub use lock::Waiting;

pub const COMMAND: &str = "home-files";

const PROGRAM: &str = "mix-daemon home-files";

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Perform(Action),
    Proceed,
    Refuse(Failure),
    Adopt(Vec<PathBuf>),
    Observe(Query),
    Active(InvokingUser),
    Generation(InvokingUser),
    Exists(PathBuf),
    FindBuilt(InvokingUser),
    WaitProfileLocks(InvokingUser),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Prepared(Vec<Action>),
    Done(Outcome),
    Built(Option<u64>),
    Observed(Option<Fact>),
    Active(Option<Active>),
    Generation(Option<PathBuf>),
    Exists(bool),
    Waiting(Waiting),
    Free,
}

pub fn path_of(action: &Action) -> Option<&Path> {
    match action {
        Action::CreateDir { path, .. }
        | Action::CreateDirs { path, .. }
        | Action::PutFile { path, .. }
        | Action::SetMode { path, .. }
        | Action::SetOwner { path, .. }
        | Action::SetAside { path, .. }
        | Action::RemoveCreated { path, .. }
        | Action::RemoveCreatedTree { path, .. }
        | Action::Restore { path, .. }
        | Action::ReclaimTree { path, .. }
        | Action::CopyTree { to: path, .. } => Some(path),
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

pub fn observe(files: &Files, query: &Query) -> Option<Fact> {
    match query {
        Query::Path(_) | Query::Contents(_) | Query::Leftovers(_) | Query::Strangers { .. } => {
            files.observe(query)
        }
        Query::Profile(_) | Query::Clobbered(_) | Query::ActiveList(_) | Query::ActiveLock(_) => {
            generation::observe(query)
        }
        _ => None,
    }
}

#[allow(clippy::disallowed_methods)]
pub fn exists(path: &Path) -> bool {
    std::fs::exists(path).unwrap_or(false)
}

pub fn refused(files: &Files, action: &Action, uid: u32) -> Option<Failure> {
    let path = match action {
        Action::SetAside { path, .. } | Action::RemoveCreatedTree { path, .. } => path,
        _ => return None,
    };
    files
        .holds_other_than(path, uid)
        .then(|| Failure::Conflict {
            subject: path.display().to_string(),
            expected: format!("a tree owned by uid {uid} alone"),
            found: "files another user owns".to_string(),
        })
}

fn theirs(user: &InvokingUser, uid: u32) -> Result<(), Failure> {
    if user.uid == uid {
        return Ok(());
    }
    Err(broken(format!(
        "a question about uid {} asked of the agent for uid {uid}",
        user.uid
    )))
}

fn subject(request: &Request) -> Option<&InvokingUser> {
    match request {
        Request::Active(user)
        | Request::Generation(user)
        | Request::FindBuilt(user)
        | Request::WaitProfileLocks(user) => Some(user),
        Request::Observe(
            Query::Profile(user)
            | Query::Clobbered(user)
            | Query::ActiveList(user)
            | Query::ActiveLock(user),
        ) => Some(user),
        _ => None,
    }
}

pub fn serve(
    request: &str,
    mut input: impl BufRead + Send,
    mut output: impl Write + Send,
) -> Result<(), Failure> {
    let uid = nix::unistd::getuid().as_raw();
    let mut files = Files::open(Path::new("/"), request).map_err(|error| Failure::Io {
        path: "/".into(),
        kind: error.kind(),
    })?;
    while let Some(request) = receive::<Request>(&mut input)? {
        if let Some(user) = subject(&request) {
            theirs(user, uid)?;
        }
        let action = match request {
            Request::Adopt(pending) => {
                files.adopt(pending);
                continue;
            }
            Request::Observe(query) => {
                send(&mut output, &Reply::Observed(observe(&files, &query)))?;
                continue;
            }
            Request::Active(user) => {
                send(&mut output, &Reply::Active(active(&user)))?;
                continue;
            }
            Request::Generation(user) => {
                send(&mut output, &Reply::Generation(active_generation(&user)))?;
                continue;
            }
            Request::Exists(path) => {
                send(&mut output, &Reply::Exists(exists(&path)))?;
                continue;
            }
            Request::FindBuilt(user) => {
                send(&mut output, &Reply::Built(built(&user)))?;
                continue;
            }
            Request::WaitProfileLocks(user) => {
                let mut sent = Ok(());
                lock::wait_blocking(&user, &mut |waiting| {
                    if sent.is_ok() {
                        sent = send(&mut output, &Reply::Waiting(waiting));
                    }
                });
                sent?;
                send(&mut output, &Reply::Free)?;
                continue;
            }
            Request::Perform(action) => action,
            other => return Err(broken(format!("{other:?} before an action"))),
        };
        let outcome = match refused(&files, &action, uid) {
            Some(failure) => Err(failure),
            None => {
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
            }
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
    pub fn spawn(
        program: &Path,
        user: &InvokingUser,
        request: &str,
        scope: &Scope,
    ) -> Result<Self, Failure> {
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
                other => return Err(unasked(&other)),
            }
        }
    }

    async fn ask(&mut self, request: &Request) -> Result<Reply, Failure> {
        self.send(request).await?;
        self.receive().await
    }

    pub async fn observe(&mut self, query: &Query) -> Result<Option<Fact>, Failure> {
        match self.ask(&Request::Observe(query.clone())).await? {
            Reply::Observed(fact) => Ok(fact),
            other => Err(unasked(&other)),
        }
    }

    pub async fn active(&mut self, user: &InvokingUser) -> Result<Option<Active>, Failure> {
        match self.ask(&Request::Active(user.clone())).await? {
            Reply::Active(active) => Ok(active),
            other => Err(unasked(&other)),
        }
    }

    pub async fn generation(&mut self, user: &InvokingUser) -> Result<Option<PathBuf>, Failure> {
        match self.ask(&Request::Generation(user.clone())).await? {
            Reply::Generation(generation) => Ok(generation),
            other => Err(unasked(&other)),
        }
    }

    pub async fn exists(&mut self, path: &Path) -> Result<bool, Failure> {
        match self.ask(&Request::Exists(path.to_path_buf())).await? {
            Reply::Exists(exists) => Ok(exists),
            other => Err(unasked(&other)),
        }
    }

    pub async fn built(&mut self, user: &InvokingUser) -> Result<Option<u64>, Failure> {
        match self.ask(&Request::FindBuilt(user.clone())).await? {
            Reply::Built(built) => Ok(built),
            other => Err(unasked(&other)),
        }
    }

    pub async fn wait_profile_locks(
        &mut self,
        user: &InvokingUser,
        on_wait: &mut (dyn FnMut(Waiting) + Send),
    ) -> Result<(), Failure> {
        self.send(&Request::WaitProfileLocks(user.clone())).await?;
        loop {
            match self.receive().await? {
                Reply::Waiting(waiting) => on_wait(waiting),
                Reply::Free => return Ok(()),
                other => return Err(unasked(&other)),
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

fn unasked(reply: &Reply) -> Failure {
    broken(format!("{reply:?} in place of the answer asked for"))
}

pub enum Home<'a> {
    Local(&'a Files),
    Agent(&'a mut Agent),
}

impl Home<'_> {
    pub async fn observe(&mut self, query: &Query) -> Result<Option<Fact>, Failure> {
        match self {
            Home::Local(files) => Ok(observe(files, query)),
            Home::Agent(agent) => agent.observe(query).await,
        }
    }

    pub async fn active(&mut self, user: &InvokingUser) -> Result<Option<Active>, Failure> {
        match self {
            Home::Local(_) => Ok(active(user)),
            Home::Agent(agent) => agent.active(user).await,
        }
    }

    pub async fn generation(&mut self, user: &InvokingUser) -> Result<Option<PathBuf>, Failure> {
        match self {
            Home::Local(_) => Ok(active_generation(user)),
            Home::Agent(agent) => agent.generation(user).await,
        }
    }

    pub async fn exists(&mut self, path: &Path) -> Result<bool, Failure> {
        match self {
            Home::Local(_) => Ok(exists(path)),
            Home::Agent(agent) => agent.exists(path).await,
        }
    }

    pub async fn built(&mut self, user: &InvokingUser) -> Result<Option<u64>, Failure> {
        match self {
            Home::Local(_) => Ok(built(user)),
            Home::Agent(agent) => agent.built(user).await,
        }
    }

    pub async fn wait_profile_locks(
        &mut self,
        user: &InvokingUser,
        on_wait: &mut (dyn FnMut(Waiting) + Send),
        scope: &Scope,
    ) -> Result<(), Failure> {
        match self {
            Home::Local(_) => lock::wait(user, on_wait, scope).await,
            Home::Agent(agent) => agent.wait_profile_locks(user, on_wait).await,
        }
    }
}

pub enum Session {
    Local(Files),
    Agent(Box<Agent>),
}

impl Session {
    pub fn open(
        program: &Path,
        user: &InvokingUser,
        request: &str,
        scope: &Scope,
    ) -> Result<Self, Failure> {
        if user.uid == nix::unistd::geteuid().as_raw() {
            return Files::open(Path::new("/"), request)
                .map(Session::Local)
                .map_err(|error| Failure::Io {
                    path: "/".into(),
                    kind: error.kind(),
                });
        }
        Agent::spawn(program, user, request, scope).map(|agent| Session::Agent(Box::new(agent)))
    }

    pub fn home(&mut self) -> Home<'_> {
        match self {
            Session::Local(files) => Home::Local(files),
            Session::Agent(agent) => Home::Agent(agent),
        }
    }

    pub async fn close(self) -> Result<(), Failure> {
        match self {
            Session::Local(_) => Ok(()),
            Session::Agent(agent) => agent.close().await,
        }
    }
}

pub fn myself() -> PathBuf {
    PathBuf::from(RUNNING_PROGRAM)
}

#[allow(clippy::disallowed_methods)]
pub fn serve_stdio(request: &str) -> Result<(), Failure> {
    let input = std::io::BufReader::new(std::io::stdin());
    let output = std::io::BufWriter::new(std::io::stdout());
    serve(request, input, output)
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

#[cfg(test)]
mod tests;
