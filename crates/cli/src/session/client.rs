use futures_util::{Stream, StreamExt};
use mix_events::Render;
use mix_events::v1::command::Request;
use mix_events::v1::{Envelope, NodeFinished, Status, envelope};
use mix_events::{Detail, Fault, ROOT};
use mix_exec::Reason;
use mix_rpc::{Client, Controller, Reply};
use nix::sys::signal::Signal;

use mix_core::paths::MIX_DAEMON_SOCKET_PATH;

use crate::args::Output;
use crate::output::{Sinks, View};
use crate::request::Route;
use crate::session::controls::{Control, Terminal, Translator};

const LAUNCHER: &str = "sudo";
const DAEMON: &str = "mix-daemon";
const SERVE_STDIN: &str = "serve-stdin";

#[derive(Debug, thiserror::Error)]
#[error("`{}` did not finish", crate::request::name(.request))]
pub struct Failed {
    pub request: Request,
    pub fault: Fault,
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
    if view.output == Output::Human {
        mix_ui::note(&crate::session::controls::detached(), None);
    }
    mix_ui::restore_terminal();
    std::process::exit(i32::try_from(crate::session::controls::DETACHED_EXIT).unwrap_or(i32::MAX));
}

fn steer(controller: &Controller, control: Control, view: &View) -> bool {
    match control {
        Control::Cancel(Reason::Terminated) => controller.send(mix_rpc::Control::Terminate),
        Control::Cancel(_) => controller.send(mix_rpc::Control::Interrupt),
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
    replies: impl Stream<Item = Result<Reply, mix_rpc::Error>>,
    view: &View,
) -> Result<Option<Fault>, mix_rpc::Error> {
    let mut terminal = Terminal::listen();
    let mut sinks: Sinks = view
        .sinks(mix_ui::display())
        .map_err(mix_rpc::Error::Spawn)?;
    let mut translator = Translator::default();
    let mut pausing = false;
    let mut replies = std::pin::pin!(replies);
    let mut ended = None;
    loop {
        tokio::select! {
            reply = replies.next() => match reply.transpose()? {
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
    ended.ok_or(mix_rpc::Error::Ended)
}

fn daemon_next_to(client: &std::path::Path) -> std::path::PathBuf {
    client.with_file_name(DAEMON)
}

async fn one_shot(view: &View) -> anyhow::Result<Client> {
    let program = daemon_next_to(&std::env::current_exe()?);
    let launcher = if nix::unistd::geteuid().is_root() {
        None
    } else {
        if view.output == Output::Human && view.level() >= Detail::Step {
            mix_ui::note(
                &mix_ui::note!("root is required, re-running with sudo"),
                None,
            );
        }
        Some(LAUNCHER)
    };
    Ok(Client::start(
        &program,
        &[SERVE_STDIN],
        launcher,
        env!("CARGO_PKG_VERSION"),
    )
    .await?)
}

fn not_set_up(request: &Request, error: &std::io::Error) -> Failed {
    Failed {
        request: request.clone(),
        fault: mix_core::diagnose::failed(
            mix_events::v1::Code::NotBootstrapped,
            format!("no mix daemon listens at {MIX_DAEMON_SOCKET_PATH}: {error}"),
            None,
        ),
    }
}

async fn socket(request: &Request) -> anyhow::Result<Client> {
    let stream = match tokio::net::UnixStream::connect(MIX_DAEMON_SOCKET_PATH).await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(not_set_up(request, &error).into());
        }
        Err(error) => return Err(mix_rpc::Error::Connect(error.to_string()).into()),
    };
    Ok(Client::connect(stream, env!("CARGO_PKG_VERSION")).await?)
}

pub async fn run(request: Request, route: Route, view: &View) -> anyhow::Result<()> {
    let mut client = match route {
        Route::OneShot => one_shot(view).await?,
        Route::Socket => socket(&request).await?,
    };
    let command = mix_events::command(request.clone());
    let (controller, replies) = client.run(&command).await?;
    let ended = replay(controller, replies, view).await?;
    let _ = client.wait().await;
    match ended {
        None => Ok(()),
        Some(fault) => Err(Failed { request, fault }.into()),
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
