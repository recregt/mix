use futures_util::{Stream, StreamExt};
use mix_events::v1::command::Request;
use mix_events::v1::{BootstrapRequest, Envelope, NodeFinished, Status, envelope};
use mix_events::{Detail, Fault, ROOT};
use mix_exec::Reason;
use mix_rpc::{Client, Controller, Reply};
use mix_shell::render::Render;
use nix::sys::signal::Signal;

use crate::cli::Output;
use crate::controls::{Control, Terminal, Translator};
use crate::render::sinks::{Sinks, View};

const LAUNCHER: &str = "sudo";
const WORKER: &str = "worker";

#[derive(Debug, thiserror::Error)]
#[error("`{}` did not finish", crate::explain::command_of(Some(.request)))]
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
        mix_ui::note(&crate::controls::detached(), None);
    }
    mix_ui::restore_terminal();
    std::process::exit(i32::try_from(crate::controls::FORCED_EXIT).unwrap_or(i32::MAX));
}

fn steer(controller: &Controller, control: Control, view: &View) -> bool {
    match control {
        Control::Cancel(Reason::Terminated) => controller.send(mix_rpc::Control::Terminate),
        Control::Cancel(_) => controller.send(mix_rpc::Control::Interrupt),
        Control::ForceStop => detach(view),
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
        .map_err(mix_rpc::Error::Spawn)?
        .detaching();
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

pub fn bootstrap_request(mirror: Option<&str>, mirror_key: Option<&str>, force: bool) -> Request {
    Request::Bootstrap(BootstrapRequest {
        force,
        mirror: mirror.map(str::to_string),
        mirror_key: mirror.and(mirror_key).map(str::to_string),
    })
}

async fn start(view: &View) -> anyhow::Result<Client> {
    if view.output == Output::Human && view.level() >= Detail::Step {
        mix_ui::note(
            &mix_ui::note!("root is required, re-running with sudo"),
            None,
        );
    }
    let program = std::env::current_exe()?;
    Ok(Client::start(
        &program,
        &[WORKER],
        Some(LAUNCHER),
        env!("CARGO_PKG_VERSION"),
    )
    .await?)
}

pub async fn run(request: Request, view: &View) -> anyhow::Result<()> {
    let mut client = start(view).await?;
    let command = crate::root::command(request.clone());
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

    #[test]
    fn the_mirror_this_process_resolved_crosses_sudo_in_the_request() {
        assert_eq!(
            bootstrap_request(Some("http://env.internal"), Some("env:KEY"), false),
            Request::Bootstrap(BootstrapRequest {
                force: false,
                mirror: Some("http://env.internal".into()),
                mirror_key: Some("env:KEY".into()),
            })
        );
        assert_eq!(
            bootstrap_request(None, Some("stray:KEY"), true),
            Request::Bootstrap(BootstrapRequest {
                force: true,
                mirror: None,
                mirror_key: None,
            })
        );
    }

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

    fn same_words<E>(errors: Vec<E>, request: Request, explain: fn(&anyhow::Error) -> String)
    where
        E: mix_events::Diagnose + std::error::Error + Send + Sync + 'static,
    {
        for error in errors {
            let fault = error.fault();
            let local = explain(&anyhow::Error::from(error));
            let remote = explain(&anyhow::Error::from(Failed {
                request: request.clone(),
                fault,
            }));
            assert_eq!(remote, local);
        }
    }

    #[test]
    fn a_failure_from_the_worker_reads_as_it_would_have_in_process() {
        use mix_shell::ops::bootstrap::{Error, Host};

        same_words(
            vec![
                Error::NotRoot("bootstrap the managed environment"),
                Error::Network("connection reset".into()),
                Error::Integrity {
                    artifact: "nix archive".into(),
                    detail: "sha256 mismatch".into(),
                },
                Error::SystemdNotReady { host: Host::Wsl },
                Error::Unit {
                    operation: "start".into(),
                    unit: "nix-daemon.socket".into(),
                    detail: "job failed".into(),
                    invocation: Some("ab12".into()),
                },
                Error::SystemdUnreachable,
                Error::AlreadyManaged,
                Error::CrossDeviceStore {
                    path: "/nix/store/pkg-a".into(),
                },
                Error::Rollback {
                    cause: Box::new(Error::UnsupportedHost),
                    summary: "1 rollback step(s) failed".into(),
                },
                Error::InvalidMirror("no scheme".into()),
            ],
            bootstrap_request(None, None, false),
            |error| crate::explain::bootstrap::explain(error).message(),
        );
        same_words(
            vec![
                mix_shell::target::Error::Unrepairable {
                    artifact: "/nix".into(),
                    reason: mix_shell::target::Unfixable::NotADirectory,
                },
                mix_shell::target::Error::Core(mix_core::Error::Locked {
                    path: "/var/lib/mix/lock".into(),
                }),
            ],
            Request::Repair(mix_events::v1::RepairRequest {}),
            |error| crate::explain::repair::explain(error).message(),
        );
    }
}
