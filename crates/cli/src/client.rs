use mix_events::Render;
use mix_events::v1::command::Request;
use mix_events::v1::{Code, Envelope, NodeFinished, Status, envelope};
use mix_events::{Fault, ROOT};
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
    Output(std::io::Error),
}

impl Failure {
    pub fn fault(&self) -> Fault {
        match self {
            Failure::Failed(fault) => (**fault).clone(),
            Failure::Transport(error) => Fault::failed(transported(error), error.to_string(), None),
            Failure::Output(error) => Fault::failed(
                Code::Io,
                format!("could not create the events file: {error}"),
                None,
            ),
        }
    }

    pub fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Failure::Transport(error) => std::error::Error::source(error),
            Failure::Failed(_) | Failure::Output(_) => None,
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

fn root(envelope: &Envelope) -> Option<&NodeFinished> {
    match &envelope.event {
        Some(envelope::Event::NodeFinished(finished)) if finished.id == ROOT => Some(finished),
        _ => None,
    }
}

fn fault_of(root: &NodeFinished) -> Option<Fault> {
    match root.status() {
        Status::Succeeded | Status::AlreadySatisfied => None,
        Status::Cancelled => Some(Fault::Cancelled {
            cause: root.cancellation(),
            rolled_back: false,
        }),
        Status::Failed | Status::Unspecified => Some(Fault::Failed(
            root.diagnostic.as_deref().cloned().unwrap_or_default(),
        )),
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

async fn replay(
    controller: Controller,
    mut replies: Replies,
    view: &View,
) -> Result<Option<Fault>, Failure> {
    let mut terminal = Terminal::listen();
    let mut sinks: Sinks = view.sinks().map_err(Failure::Output)?;
    let mut translator = Translator::default();
    let mut pausing = false;
    let mut ended = None;
    loop {
        tokio::select! {
            reply = replies.next() => match reply.transpose().map_err(Failure::Transport)? {
                None => break,
                Some(Reply::Envelope(envelope)) => {
                    if let Some(finished) = root(&envelope) {
                        ended = Some(fault_of(finished));
                    }
                    sinks.envelope(envelope);
                }
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
    ended.ok_or(Failure::Transport(mix_rpc::Error::Ended))
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

fn not_set_up(error: &std::io::Error) -> Failure {
    Failure::Failed(Box::new(Fault::failed(
        Code::NotBootstrapped,
        format!("no mix daemon listens at {}: {error}", mix_rpc::SOCKET_PATH),
        None,
    )))
}

async fn socket() -> Result<Client, Failure> {
    let stream = match tokio::net::UnixStream::connect(mix_rpc::SOCKET_PATH).await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(not_set_up(&error));
        }
        Err(error) => {
            return Err(Failure::Transport(mix_rpc::Error::Connect(
                error.to_string(),
            )));
        }
    };
    Client::connect(stream, env!("CARGO_PKG_VERSION"))
        .await
        .map_err(Failure::Transport)
}

pub async fn run(request: &Request, route: Route, view: &View) -> Result<(), Failure> {
    let mut client = match route {
        Route::OneShot => one_shot(view).await.map_err(Failure::Transport)?,
        Route::Socket => socket().await?,
    };
    let command = mix_events::command(request.clone());
    let (controller, replies) = client.run(&command).await.map_err(Failure::Transport)?;
    let ended = replay(controller, replies, view).await?;
    let _ = client.wait().await;
    match ended {
        None => Ok(()),
        Some(fault) => Err(Failure::Failed(Box::new(fault))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(status: Status) -> NodeFinished {
        NodeFinished {
            id: ROOT,
            status: status as i32,
            ..NodeFinished::default()
        }
    }

    #[test]
    fn only_a_root_that_succeeded_or_was_already_satisfied_is_not_a_fault() {
        assert_eq!(fault_of(&finished(Status::Succeeded)), None);
        assert_eq!(fault_of(&finished(Status::AlreadySatisfied)), None);
        assert!(matches!(
            fault_of(&finished(Status::Failed)),
            Some(Fault::Failed(_))
        ));
        assert!(matches!(
            fault_of(&finished(Status::Cancelled)),
            Some(Fault::Cancelled { .. })
        ));
    }
}
