use std::process::ExitCode;
use std::sync::Arc;

use mix_core::paths::LOCK_FILE;
use mix_events::v1::{Command, Envelope};
use mix_rpc::{Caller, Controls, Events, Reply};
use mix_shell::request::lock::Locks;
use mix_shell::request::sink::Render;

use crate::controls;

struct Forward(Events);

impl Render for Forward {
    fn envelope(&mut self, envelope: Envelope) {
        let _ = self.0.send(Reply::Envelope(envelope));
    }

    fn detail(&self) -> mix_events::Detail {
        mix_events::Detail::Trace
    }
}

#[derive(Clone, Copy)]
pub enum Gate {
    Sudo,
    Members,
}

#[derive(Clone)]
pub struct Host {
    pub gate: Gate,
    pub locks: Arc<Locks>,
}

impl Host {
    pub fn new(gate: Gate) -> Self {
        Self::at(gate, LOCK_FILE)
    }

    pub fn at(gate: Gate, lock: impl Into<std::path::PathBuf>) -> Self {
        Self {
            gate,
            locks: Arc::new(Locks::new(lock)),
        }
    }

    fn session(&self, caller: Caller, events: &Events) -> mix_shell::Session {
        let account = if caller.uid == 0 {
            mix_shell::effect::accounts::invoking_user(crate::env::sudo_uid().as_deref())
        } else {
            mix_shell::effect::accounts::user_by_uid(caller.uid)
        };
        mix_shell::Session::new(mix_exec::Scope::root())
            .with_render(Forward(events.clone()))
            .with_locks(Arc::clone(&self.locks))
            .with_caller(mix_shell::Caller::Account {
                peer_is_root: caller.uid == 0,
                account,
            })
    }
}

fn enrolled(uid: u32) -> bool {
    uid == 0
        || mix_shell::effect::accounts::user_by_uid(uid).is_some_and(|user| {
            mix_shell::effect::accounts::user_in_group(mix_core::identity::MIX_USERS_GROUP, &user)
        })
}

impl mix_rpc::Worker for Host {
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    fn admits(&self, caller: Caller) -> bool {
        match self.gate {
            Gate::Sudo => true,
            Gate::Members => enrolled(caller.uid),
        }
    }

    async fn run(&self, caller: Caller, command: Command, controls: Controls, events: Events) {
        let session = self.session(caller, &events);
        let _steer = controls::steer(&session.scope, controls, &events);
        mix_shell::request::run(&session, command).await;
    }
}

pub async fn serve_stdin() -> ExitCode {
    if !mix_shell::effect::accounts::is_root() {
        mix_ui::report(
            mix_ui::Severity::Error,
            &mix_ui::Report::new(&mix_ui::phrase!(
                "the worker must be started by `mix` itself, as root"
            )),
        );
        return ExitCode::FAILURE;
    }
    tokio::spawn(controls::ignore_the_terminal());
    match mix_rpc::serve_stdin(Host::new(Gate::Sudo)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            mix_ui::report(
                mix_ui::Severity::Error,
                &mix_ui::Report::new(&mix_ui::phrase!("the worker stopped serving its client"))
                    .causes(vec![error.to_string()]),
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use mix_events::ROOT;
    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, DoctorRequest, NodeFinished, envelope, node_finished};

    use super::*;

    async fn served(command: Command) -> Vec<Envelope> {
        let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
        let lock = tempfile::tempdir().unwrap();
        let server = tokio::spawn(mix_rpc::serve_connection(
            Host::at(Gate::Sudo, lock.path().join("lock")),
            theirs,
        ));
        let mut client = mix_rpc::Client::connect(ours, env!("CARGO_PKG_VERSION"))
            .await
            .unwrap();
        let (controls, replies) = client.run(&command).await.unwrap();
        let envelopes = replies
            .filter_map(|reply| async move {
                match reply.unwrap() {
                    Reply::Envelope(envelope) => Some(envelope),
                    Reply::Applied(_) => None,
                }
            })
            .collect()
            .await;
        drop(controls);
        drop(client);
        server.await.unwrap().unwrap();
        envelopes
    }

    fn root(envelopes: &[Envelope]) -> &NodeFinished {
        envelopes
            .iter()
            .find_map(|envelope| match &envelope.event {
                Some(envelope::Event::NodeFinished(finished)) if finished.id == ROOT => {
                    Some(finished)
                }
                _ => None,
            })
            .expect("every request ends with its root")
    }

    #[tokio::test]
    async fn a_doctor_request_ends_with_the_reports_in_its_root() {
        let envelopes = served(mix_events::command(Request::Doctor(DoctorRequest {}))).await;

        assert!(matches!(
            root(&envelopes).result,
            Some(node_finished::Result::Doctor(_))
        ));
        assert!(mix_events::validate(&envelopes).is_ok());
    }

    #[tokio::test]
    async fn a_request_that_names_no_command_still_ends_with_a_failed_root() {
        let envelopes = served(Command::default()).await;

        let root = root(&envelopes);
        assert_eq!(root.status(), mix_events::v1::Status::Failed);
        assert_eq!(
            root.diagnostic
                .as_deref()
                .map(|diagnostic| diagnostic.code()),
            Some(Code::Internal)
        );
        assert!(mix_events::validate(&envelopes).is_ok());
    }
}

#[cfg(test)]
mod gate_tests {
    use mix_rpc::Worker;

    use super::*;

    #[test]
    fn the_socket_admits_root_and_refuses_a_user_outside_mix_users() {
        let host = Host::at(Gate::Members, "/nonexistent/lock");

        assert!(host.admits(Caller { uid: 0, gid: 0 }));
        assert!(!host.admits(Caller {
            uid: u32::MAX - 1,
            gid: u32::MAX - 1
        }));
    }

    #[test]
    fn the_one_shot_worker_admits_whoever_sudo_let_through() {
        let host = Host::at(Gate::Sudo, "/nonexistent/lock");

        assert!(host.admits(Caller {
            uid: u32::MAX - 1,
            gid: u32::MAX - 1
        }));
    }
}
