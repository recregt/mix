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

fn unreachable(socket: &Path, error: &std::io::Error) -> Failure {
    match error.kind() {
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

async fn socket(path: &Path) -> Result<Client, Failure> {
    let stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(|error| unreachable(path, &error))?;
    Client::connect(stream, env!("CARGO_PKG_VERSION"))
        .await
        .map_err(Failure::Transport)
}

pub async fn run(
    command: &Command,
    route: Route,
    view: &View,
    socket_path: &Path,
) -> Result<(), Failure> {
    let mut client = match route {
        Route::OneShot => one_shot(view).await.map_err(Failure::Transport)?,
        Route::Socket => socket(socket_path).await?,
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
    fn a_missing_or_silent_socket_means_mix_is_not_set_up_and_anything_else_is_transport() {
        let socket = Path::new("/run/mix/daemon.sock");
        for (kind, code) in [
            (std::io::ErrorKind::NotFound, Code::NotBootstrapped),
            (std::io::ErrorKind::ConnectionRefused, Code::NotBootstrapped),
            (
                std::io::ErrorKind::PermissionDenied,
                Code::PrivilegesUnavailable,
            ),
        ] {
            let failure = unreachable(socket, &kind.into());
            assert_eq!(failure.fault().code(), Some(code), "{kind:?}");
        }
    }
}
