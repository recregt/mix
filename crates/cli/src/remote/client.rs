use futures_util::{Stream, StreamExt};
use mix_events::v1::command::Request;
use mix_events::v1::{BootstrapRequest, Envelope, NodeFinished, Status, envelope};
use mix_events::{Detail, Fault, ROOT};
use mix_rpc::Client;
use mix_shell::render::Render;

use crate::cli::Output;
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

async fn replay(
    envelopes: impl Stream<Item = Result<Envelope, mix_rpc::Error>>,
    view: &View,
) -> Result<Option<Fault>, mix_rpc::Error> {
    let interrupts = tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    let mut sinks: Sinks = view
        .sinks(mix_ui::display())
        .map_err(mix_rpc::Error::Spawn)?;
    let mut envelopes = std::pin::pin!(envelopes);
    let mut ended = None;
    while let Some(envelope) = envelopes.next().await {
        let envelope = envelope?;
        if let Some(finished) = root(&envelope) {
            ended = Some(fault_of(finished));
        }
        sinks.envelope(envelope);
    }
    interrupts.abort();
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
    Ok(Client::start(&program, &[WORKER], Some(LAUNCHER)).await?)
}

pub async fn run(request: Request, view: &View) -> anyhow::Result<()> {
    let mut client = start(view).await?;
    let command = crate::root::command(request.clone());
    let ended = replay(client.run(&command).await?, view).await?;
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
