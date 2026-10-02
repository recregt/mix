use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mix_core::paths::LOCK_FILE;
use mix_events::v1::command::Request;
use mix_events::v1::{BootstrapRequest, Code, Command, Envelope, envelope};
use mix_events::{Diagnose, Fault, ROOT};
use mix_rpc::{Caller, Controls, Events, Reply};
use mix_shell::render::Render;

use crate::controls;

#[derive(Clone)]
struct Forward {
    events: Events,
    started: Arc<AtomicBool>,
}

impl Render for Forward {
    fn envelope(&mut self, envelope: Envelope) {
        if let Some(envelope::Event::NodeStarted(started)) = &envelope.event
            && started.id == ROOT
        {
            self.started.store(true, Ordering::SeqCst);
        }
        let _ = self.events.send(Reply::Envelope(envelope));
    }

    fn detail(&self) -> mix_events::Detail {
        mix_events::Detail::Trace
    }
}

fn fault(error: impl Diagnose) -> Box<Fault> {
    Box::new(error.fault())
}

fn context(user: Option<mix_core::models::UserConfig>, forward: &Forward) -> mix_shell::Context {
    mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(user)
        .with_render(forward.clone())
        .with_host(crate::settings::host_config())
}

async fn bootstrap(
    caller: Caller,
    request: BootstrapRequest,
    controls: Controls,
    forward: &Forward,
) -> Result<(), Box<Fault>> {
    let _lock = mix_shell::effect::lock::acquire_exclusive(LOCK_FILE).map_err(fault)?;
    let policy =
        crate::settings::requested_policy(request.mirror.as_deref(), request.mirror_key.as_deref())
            .map_err(fault)?;
    let account = if caller.uid == 0 {
        mix_shell::effect::accounts::invoking_user()
    } else {
        mix_shell::effect::accounts::user_by_uid(caller.uid)
    };
    let user = account.and_then(mix_shell::profile::user_config_for);
    let ctx = context(user, forward).with_policy(policy);
    let _steer = controls::steer(&ctx.scope, controls, &forward.events);
    mix_shell::ops::bootstrap::bootstrap(&ctx, request.force)
        .await
        .map(drop)
        .map_err(fault)
}

async fn repair(caller: Caller, controls: Controls, forward: &Forward) -> Result<(), Box<Fault>> {
    let _lock = mix_shell::effect::lock::acquire_exclusive(LOCK_FILE).map_err(fault)?;
    let user = mix_shell::effect::accounts::user_by_uid(caller.uid)
        .and_then(mix_shell::profile::existing_user_config_for);
    let ctx = context(user, forward).with_policy(crate::settings::policy());
    let _steer = controls::steer(&ctx.scope, controls, &forward.events);
    mix_shell::ops::repair::repair(&ctx).await;
    Ok(())
}

enum Change {
    Install,
    Remove,
}

async fn change(
    caller: Caller,
    verb: Change,
    packages: &[String],
    controls: Controls,
    forward: &Forward,
) -> Result<(), Box<Fault>> {
    if caller.uid == 0 {
        return Err(fault(mix_shell::profile::change::Error::NotRoot));
    }
    let _lock = mix_shell::effect::lock::acquire_exclusive(LOCK_FILE).map_err(fault)?;
    let user = mix_shell::effect::accounts::user_by_uid(caller.uid)
        .and_then(mix_shell::profile::existing_user_config_for);
    let ctx = context(user, forward).with_policy(crate::settings::policy());
    let _steer = controls::steer(&ctx.scope, controls, &forward.events);
    match verb {
        Change::Install => mix_shell::ops::install::install(&ctx, packages)
            .await
            .map(drop)
            .map_err(fault),
        Change::Remove => mix_shell::ops::remove::remove(&ctx, packages)
            .await
            .map(drop)
            .map_err(fault),
    }
}

async fn doctor(caller: Caller, controls: Controls, forward: &Forward) -> Result<(), Box<Fault>> {
    let user = mix_shell::effect::accounts::user_by_uid(caller.uid)
        .and_then(mix_shell::profile::existing_user_config_for);
    let ctx = context(user, forward).with_policy(crate::settings::policy());
    let _steer = controls::steer(&ctx.scope, controls, &forward.events);
    mix_shell::ops::doctor::audit(&ctx).await;
    Ok(())
}

fn unserved() -> Fault {
    mix_core::diagnose::failed(Code::Internal, "the request names no command", None)
}

#[derive(Clone, Copy)]
pub enum Gate {
    Sudo,
    Members,
}

#[derive(Clone, Copy)]
pub struct Host {
    pub gate: Gate,
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
        let mut forward = Forward {
            events,
            started: Arc::new(AtomicBool::new(false)),
        };
        let ran = match command.request.clone() {
            Some(Request::Bootstrap(request)) => {
                bootstrap(caller, *request, controls, &forward).await
            }
            Some(Request::Repair(_)) => repair(caller, controls, &forward).await,
            Some(Request::Install(request)) => {
                change(
                    caller,
                    Change::Install,
                    &request.packages,
                    controls,
                    &forward,
                )
                .await
            }
            Some(Request::Remove(request)) => {
                change(
                    caller,
                    Change::Remove,
                    &request.packages,
                    controls,
                    &forward,
                )
                .await
            }
            Some(Request::Doctor(_)) => doctor(caller, controls, &forward).await,
            None => Err(Box::new(unserved())),
        };
        if let Err(fault) = ran
            && !forward.started.load(Ordering::SeqCst)
        {
            mix_shell::root::fail(command, *fault, &mut forward);
        }
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
    match mix_rpc::serve_stdin(Host { gate: Gate::Sudo }).await {
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
    use mix_events::v1::{DoctorRequest, NodeFinished, node_finished};

    use super::*;

    async fn served(command: Command) -> Vec<Envelope> {
        let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
        let server = tokio::spawn(mix_rpc::serve_connection(Host { gate: Gate::Sudo }, theirs));
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
        let envelopes = served(mix_shell::root::command(Request::Doctor(DoctorRequest {}))).await;

        assert!(matches!(
            root(&envelopes).result,
            Some(node_finished::Result::Doctor(_))
        ));
    }

    #[tokio::test]
    async fn a_request_that_never_started_still_ends_with_a_failed_root() {
        let envelopes = served(Command::default()).await;

        let root = root(&envelopes);
        assert_eq!(root.status(), mix_events::v1::Status::Failed);
        assert_eq!(
            root.diagnostic
                .as_deref()
                .map(|diagnostic| diagnostic.code()),
            Some(Code::Internal)
        );
        assert!(
            mix_events::validate(&envelopes).is_ok(),
            "the fallback root is a valid stream"
        );
    }

    #[tokio::test]
    async fn root_is_refused_a_package_change_before_anything_is_locked() {
        let (events, _received) = tokio::sync::mpsc::unbounded_channel();
        let forward = Forward {
            events,
            started: Arc::new(AtomicBool::new(false)),
        };

        let refused = change(
            Caller { uid: 0, gid: 0 },
            Change::Install,
            &["hello".to_string()],
            tokio::sync::mpsc::unbounded_channel().1,
            &forward,
        )
        .await
        .unwrap_err();

        assert_eq!(refused.code(), Some(Code::RootNotAllowed));
    }
}

#[cfg(test)]
mod gate_tests {
    use mix_rpc::Worker;

    use super::*;

    #[test]
    fn the_socket_admits_root_and_refuses_a_user_outside_mix_users() {
        let host = Host {
            gate: Gate::Members,
        };

        assert!(host.admits(Caller { uid: 0, gid: 0 }));
        assert!(!host.admits(Caller {
            uid: u32::MAX - 1,
            gid: u32::MAX - 1
        }));
    }

    #[test]
    fn the_one_shot_worker_admits_whoever_sudo_let_through() {
        let host = Host { gate: Gate::Sudo };

        assert!(host.admits(Caller {
            uid: u32::MAX - 1,
            gid: u32::MAX - 1
        }));
    }
}
