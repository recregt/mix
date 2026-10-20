use std::path::Path;

use mix_events::Fault;
use mix_events::Render;
use mix_events::v1::Code;
use mix_events::v1::Command;
use mix_render::{Sinks, View};
use mix_rpc::{Client, Controller, Replies, Reply};
use nix::sys::signal::Signal;

use crate::controls::{Control, Terminal, Translator};
use crate::request::Route;

const LAUNCHER: &str = "sudo";
const DAEMON: &str = "mix-daemon";
const SERVE_STDIN: &str = "serve-stdin";

#[derive(Debug)]
pub enum Failure {
    Failed(Box<Fault>),
    Transport(mix_rpc::Error),
}

impl Failure {
    pub fn fault(&self) -> Fault {
        match self {
            Failure::Failed(fault) => (**fault).clone(),
            Failure::Transport(error) => Fault::failed(transported(error), error.to_string(), None),
        }
    }

    pub fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Failure::Transport(error) => std::error::Error::source(error),
            Failure::Failed(_) => None,
        }
    }
}

fn transported(error: &mix_rpc::Error) -> Code {
    use mix_rpc::Error;

    match error {
        Error::Spawn(_) | Error::Launch(_) | Error::Connect(_) | Error::Refused(_) => {
            Code::PrivilegesUnavailable
        }
        Error::Ended => Code::WorkerEnded,
        Error::VersionMismatch { .. } => Code::VersionMismatch,
        Error::Outdated { .. } => Code::DaemonOutdated,
        Error::Denied(_) => Code::NotBootstrapped,
        Error::Malformed(_) | Error::NotAConnection(_) => Code::Internal,
    }
}

fn detach(view: &View) -> ! {
    view.detaching();
    mix_render::restore();
    std::process::exit(i32::try_from(crate::controls::DETACHED_EXIT).unwrap_or(i32::MAX));
}

fn steer(controller: &Controller, control: Control, view: &View) -> bool {
    match control {
        Control::Interrupt => controller.send(mix_rpc::Control::Interrupt),
        Control::Terminate => controller.send(mix_rpc::Control::Terminate),
        Control::Detach => detach(view),
        Control::Pause => {
            controller.send(mix_rpc::Control::Pause);
            return true;
        }
        Control::Resume => controller.send(mix_rpc::Control::Resume),
    }
    false
}

async fn replay(controller: Controller, mut replies: Replies, view: &View) -> Result<(), Failure> {
    let mut terminal = Terminal::listen();
    let mut sinks: Sinks = view.sinks();
    let mut translator = Translator::default();
    let mut pausing = false;
    loop {
        tokio::select! {
            reply = replies.next() => match reply.transpose().map_err(Failure::Transport)? {
                None => break,
                Some(Reply::Envelope(envelope)) => sinks.envelope(envelope),
                Some(Reply::Applied(mix_rpc::Control::Pause)) if std::mem::take(&mut pausing) => {
                    let _ = nix::sys::signal::raise(Signal::SIGSTOP);
                }
                Some(Reply::Applied(_)) => {}
            },
            received = terminal.next() => {
                pausing |= steer(&controller, translator.translate(received), view);
            }
        }
    }
    match view.exit.code() {
        Some(_) => Ok(()),
        None => Err(Failure::Transport(mix_rpc::Error::Ended)),
    }
}

fn daemon_next_to(client: &std::path::Path) -> std::path::PathBuf {
    client.with_file_name(DAEMON)
}

async fn one_shot(view: &View) -> Result<Client, mix_rpc::Error> {
    let program = daemon_next_to(&std::env::current_exe().map_err(mix_rpc::Error::Spawn)?);
    let launcher = if nix::unistd::geteuid().is_root() {
        None
    } else {
        view.escalating();
        Some(LAUNCHER)
    };
    Client::start(
        &program,
        &[SERVE_STDIN],
        launcher,
        env!("CARGO_PKG_VERSION"),
    )
    .await
}

fn daemon_unavailable(message: String) -> Failure {
    Failure::Failed(Box::new(Fault::failed(
        Code::DaemonUnavailable,
        message,
        None,
    )))
}

fn unreachable(socket: &Path, installed: bool, error: &std::io::Error) -> Failure {
    match error.kind() {
        _ if installed => daemon_unavailable(format!(
            "no mix daemon answers at {}: {error}",
            socket.display()
        )),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            Failure::Failed(Box::new(Fault::failed(
                Code::NotBootstrapped,
                format!("no mix daemon listens at {}: {error}", socket.display()),
                None,
            )))
        }
        _ => Failure::Transport(mix_rpc::Error::Connect(error.to_string())),
    }
}

fn unanswered(socket: &Path, error: mix_rpc::Error) -> Failure {
    match error {
        mix_rpc::Error::Connect(_) | mix_rpc::Error::Refused(_) | mix_rpc::Error::Ended => {
            daemon_unavailable(format!(
                "the mix daemon at {} closed the connection before it answered: {error}",
                socket.display()
            ))
        }
        error => Failure::Transport(error),
    }
}

pub struct Daemon {
    pub socket: std::path::PathBuf,
    pub unit: std::path::PathBuf,
}

impl Daemon {
    pub fn installed() -> Self {
        Self {
            socket: mix_rpc::SOCKET_PATH.into(),
            unit: mix_rpc::SOCKET_UNIT_FILE.into(),
        }
    }

    #[cfg(test)]
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            socket: dir.join("daemon.sock"),
            unit: dir.join("mix-daemon.socket"),
        }
    }
}

#[allow(clippy::disallowed_methods)]
async fn socket(daemon: &Daemon) -> Result<Client, Failure> {
    let stream = tokio::net::UnixStream::connect(&daemon.socket)
        .await
        .map_err(|error| unreachable(&daemon.socket, daemon.unit.exists(), &error))?;
    Client::connect(stream, env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|error| unanswered(&daemon.socket, error))
}

pub async fn run(
    command: &Command,
    route: Route,
    view: &View,
    daemon: &Daemon,
) -> Result<(), Failure> {
    let mut client = match route {
        Route::OneShot => one_shot(view).await.map_err(Failure::Transport)?,
        Route::Socket => socket(daemon).await?,
    };
    let (controller, replies) = client.run(command).await.map_err(Failure::Transport)?;
    let replayed = replay(controller, replies, view).await;
    let _ = client.wait().await;
    replayed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_one_shot_worker_is_the_daemon_installed_next_to_mix() {
        assert_eq!(
            daemon_next_to(Path::new("/usr/local/bin/mix")),
            Path::new("/usr/local/bin/mix-daemon")
        );
    }

    fn io(message: &str) -> std::io::Error {
        std::io::Error::other(message.to_string())
    }

    #[test]
    fn every_failure_has_the_code_its_reader_acts_on() {
        use mix_rpc::Error;

        let daemon = Fault::failed(Code::RootNotAllowed, "root", None);
        for (failure, code) in [
            (Failure::Failed(Box::new(daemon)), Code::RootNotAllowed),
            (
                Failure::Transport(Error::Spawn(io("x"))),
                Code::PrivilegesUnavailable,
            ),
            (
                Failure::Transport(Error::Launch(mix_exec::Error::Spawn {
                    command: "sudo".into(),
                    source: io("x"),
                })),
                Code::PrivilegesUnavailable,
            ),
            (
                Failure::Transport(Error::Connect("x".into())),
                Code::PrivilegesUnavailable,
            ),
            (
                Failure::Transport(Error::Refused("x".into())),
                Code::PrivilegesUnavailable,
            ),
            (Failure::Transport(Error::Ended), Code::WorkerEnded),
            (
                Failure::Transport(Error::Denied("x".into())),
                Code::NotBootstrapped,
            ),
            (
                Failure::Transport(Error::VersionMismatch {
                    ours: "1.0.0".into(),
                    theirs: "1.1.0".into(),
                }),
                Code::VersionMismatch,
            ),
            (
                Failure::Transport(Error::Outdated { ours: 7, theirs: 0 }),
                Code::DaemonOutdated,
            ),
            (
                Failure::Transport(Error::Malformed(mix_rpc::Malformed("x".into()))),
                Code::Internal,
            ),
            (
                Failure::Transport(Error::NotAConnection(io("x"))),
                Code::Internal,
            ),
        ] {
            assert_eq!(failure.fault().code(), Some(code), "{failure:?}");
        }
    }

    #[test]
    fn a_transport_failure_hands_its_cause_to_the_report() {
        let failure = Failure::Transport(mix_rpc::Error::Spawn(io("sudo: command not found")));

        assert_eq!(
            failure.source().map(ToString::to_string).as_deref(),
            Some("sudo: command not found")
        );
    }

    #[test]
    fn an_unreachable_socket_is_a_daemon_problem_once_mix_is_installed() {
        let socket = Path::new("/run/mix/daemon.sock");
        for (kind, installed, code) in [
            (std::io::ErrorKind::NotFound, false, Code::NotBootstrapped),
            (
                std::io::ErrorKind::ConnectionRefused,
                false,
                Code::NotBootstrapped,
            ),
            (
                std::io::ErrorKind::PermissionDenied,
                false,
                Code::PrivilegesUnavailable,
            ),
            (std::io::ErrorKind::NotFound, true, Code::DaemonUnavailable),
            (
                std::io::ErrorKind::ConnectionRefused,
                true,
                Code::DaemonUnavailable,
            ),
            (
                std::io::ErrorKind::PermissionDenied,
                true,
                Code::DaemonUnavailable,
            ),
            (
                std::io::ErrorKind::WouldBlock,
                true,
                Code::DaemonUnavailable,
            ),
        ] {
            let failure = unreachable(socket, installed, &kind.into());
            assert_eq!(failure.fault().code(), Some(code), "{kind:?} {installed}");
        }
    }

    #[test]
    fn a_daemon_that_drops_the_connection_is_not_a_missing_right() {
        use mix_rpc::Error;

        let socket = Path::new("/run/mix/daemon.sock");
        for (error, code) in [
            (
                Error::Connect("connection reset".into()),
                Code::DaemonUnavailable,
            ),
            (
                Error::Refused("h2 protocol error".into()),
                Code::DaemonUnavailable,
            ),
            (Error::Ended, Code::DaemonUnavailable),
            (Error::Denied("uid 1000".into()), Code::NotBootstrapped),
            (
                Error::VersionMismatch {
                    ours: "0.1.0".into(),
                    theirs: "0.0.9".into(),
                },
                Code::VersionMismatch,
            ),
        ] {
            let shown = format!("{error:?}");
            assert_eq!(
                unanswered(socket, error).fault().code(),
                Some(code),
                "{shown}"
            );
        }
    }
}
