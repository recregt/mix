use std::process::ExitCode;
use std::sync::Arc;

use std::sync::Mutex;

use mix_core::paths::LOCK_FILE;
use mix_events::ROOT;
use mix_events::v1::{Command, Envelope, NodeFinished, envelope};
use mix_rpc::{Caller, Controls, Events, Reply};
use mix_shell::request::lock::Locks;
use mix_shell::request::sink::Render;

use crate::controls;

type Ending = Arc<Mutex<Option<(String, NodeFinished)>>>;

struct Forward(Events, Ending, crate::journal::Steps);

impl Render for Forward {
    fn envelope(&mut self, envelope: Envelope) {
        for entry in self.2.entries(&envelope) {
            crate::journal::record(&entry);
        }
        if let Some(envelope::Event::NodeFinished(finished)) = &envelope.event
            && finished.id == ROOT
        {
            *self
                .1
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((envelope.request.clone(), finished.clone()));
        }
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

    fn session(
        &self,
        caller: Caller,
        account: Option<mix_core::identity::InvokingUser>,
        events: &Events,
        ending: &Ending,
        steps: crate::journal::Steps,
    ) -> mix_shell::Session {
        mix_shell::Session::new(mix_exec::Scope::root())
            .with_render(Forward(events.clone(), Arc::clone(ending), steps))
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
        let account = if caller.uid == 0 {
            mix_shell::effect::accounts::invoking_user(crate::env::sudo_uid().as_deref())
        } else {
            mix_shell::effect::accounts::user_by_uid(caller.uid)
        };
        let who = account.as_ref().map_or_else(
            || format!("uid {}", caller.uid),
            |account| account.name.clone(),
        );
        let key = mix_events::key_of(command.request.as_ref());
        let ending = Ending::default();
        let steps = crate::journal::Steps::new(&who, key);
        let session = self.session(caller, account, &events, &ending, steps);
        let _steer = controls::steer(&session.scope, controls, &events);
        mix_shell::request::run(&session, command).await;
        let ended = ending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        crate::journal::record(&audit(&who, key, ended.as_ref()));
    }
}

fn audit(
    who: &str,
    key: &str,
    ended: Option<&(String, NodeFinished)>,
) -> Vec<(&'static str, String)> {
    let outcome = ended.map_or_else(
        || "ended without an outcome".to_string(),
        |(_, finished)| crate::journal::status_words(finished.status()),
    );
    let failed =
        !ended.is_some_and(|(_, finished)| finished.exit_code == mix_events::exit::SUCCEEDED);
    let mut fields = vec![
        ("MESSAGE", format!("{who} ran mix {key}: {outcome}")),
        ("PRIORITY", if failed { "4" } else { "6" }.to_string()),
        ("SYSLOG_IDENTIFIER", "mix-daemon".to_string()),
        ("MIX_USER", who.to_string()),
        ("MIX_COMMAND", key.to_string()),
    ];
    if let Some((request, finished)) = ended {
        fields.push(("MIX_REQUEST", request.clone()));
        fields.push(("MIX_EXIT", finished.exit_code.to_string()));
        if let Some(diagnostic) = &finished.diagnostic {
            fields.push((
                "MIX_CODE",
                mix_events::code::name(diagnostic.code()).to_string(),
            ));
        }
    }
    fields
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
    use mix_events::v1::Code;
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

    fn finished(
        status: mix_events::v1::Status,
        exit_code: u32,
        code: Option<Code>,
    ) -> (String, NodeFinished) {
        (
            "request-1".to_string(),
            NodeFinished {
                id: ROOT,
                status: status as i32,
                exit_code,
                diagnostic: code.map(|code| {
                    Box::new(mix_events::v1::Diagnostic {
                        code: code as i32,
                        ..Default::default()
                    })
                }),
                ..Default::default()
            },
        )
    }

    fn field<'a>(fields: &'a [(&str, String)], name: &str) -> Option<&'a str> {
        fields
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn an_audit_entry_names_who_ran_what_and_how_it_ended() {
        let succeeded = audit(
            "alice",
            "install",
            Some(&finished(mix_events::v1::Status::Succeeded, 0, None)),
        );
        assert_eq!(
            field(&succeeded, "MESSAGE"),
            Some("alice ran mix install: succeeded")
        );
        assert_eq!(field(&succeeded, "PRIORITY"), Some("6"));
        assert_eq!(field(&succeeded, "MIX_REQUEST"), Some("request-1"));
        assert_eq!(field(&succeeded, "MIX_CODE"), None);

        let failed = audit(
            "uid 1001",
            "repair",
            Some(&finished(
                mix_events::v1::Status::Failed,
                mix_events::exit::FAILED,
                Some(Code::NotBootstrapped),
            )),
        );
        assert_eq!(field(&failed, "PRIORITY"), Some("4"));
        assert_eq!(field(&failed, "MIX_CODE"), Some("NOT_BOOTSTRAPPED"));
        assert_eq!(field(&failed, "MIX_EXIT"), Some("1"));
    }

    #[test]
    fn a_request_that_ended_without_an_outcome_is_still_audited() {
        let fields = audit("alice", "doctor", None);

        assert_eq!(
            field(&fields, "MESSAGE"),
            Some("alice ran mix doctor: ended without an outcome")
        );
        assert_eq!(field(&fields, "PRIORITY"), Some("4"));
        assert_eq!(field(&fields, "MIX_REQUEST"), None);
    }
}
