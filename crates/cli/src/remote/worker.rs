use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mix_core::paths::LOCK_FILE;
use mix_events::v1::command::Request;
use mix_events::v1::{BootstrapRequest, Code, Command, Envelope, envelope};
use mix_events::{Diagnose, Fault, ROOT};
use mix_rpc::{Caller, Events};
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
        let _ = self.events.send(envelope);
    }

    fn detail(&self) -> mix_events::Detail {
        mix_events::Detail::Trace
    }
}

fn fault(error: impl Diagnose) -> Box<Fault> {
    Box::new(error.fault())
}

fn client_gone(events: &Events) -> impl Future<Output = ()> + Send + 'static {
    let events = events.clone();
    async move { events.closed().await }
}

fn context(user: Option<mix_core::models::UserConfig>, forward: &Forward) -> mix_shell::Context {
    mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(user)
        .with_render(forward.clone())
        .with_host(crate::commands::host_config())
}

async fn bootstrap(
    caller: Caller,
    request: BootstrapRequest,
    forward: &Forward,
) -> Result<(), Box<Fault>> {
    let _lock = mix_shell::effect::lock::acquire_exclusive(LOCK_FILE).map_err(fault)?;
    let policy =
        crate::commands::requested_policy(request.mirror.as_deref(), request.mirror_key.as_deref())
            .map_err(fault)?;
    let user = mix_shell::effect::accounts::user_by_uid(caller.uid)
        .and_then(mix_shell::profile::user_config_for);
    let ctx = context(user, forward).with_policy(policy);
    let _watch = controls::watch(
        &ctx.scope,
        None,
        client_gone(&forward.events),
        controls::Side::Worker,
    );
    mix_shell::ops::bootstrap::bootstrap(&ctx, request.force)
        .await
        .map(drop)
        .map_err(fault)
}

async fn repair(caller: Caller, forward: &Forward) -> Result<(), Box<Fault>> {
    let _lock = mix_shell::effect::lock::acquire_exclusive(LOCK_FILE).map_err(fault)?;
    let user = mix_shell::effect::accounts::user_by_uid(caller.uid)
        .and_then(mix_shell::profile::existing_user_config_for);
    let ctx = context(user, forward).with_policy(crate::commands::policy());
    let _watch = controls::watch(
        &ctx.scope,
        None,
        client_gone(&forward.events),
        controls::Side::Worker,
    );
    mix_shell::ops::repair::repair(&ctx).await;
    Ok(())
}

fn unserved(command: &Command) -> Fault {
    mix_core::diagnose::failed(
        Code::Internal,
        format!(
            "the worker does not serve `{}`",
            crate::root::key_of(command.request.as_ref())
        ),
        None,
    )
}

struct CliWorker;

impl mix_rpc::Worker for CliWorker {
    async fn run(&self, caller: Caller, command: Command, events: Events) {
        let mut forward = Forward {
            events,
            started: Arc::new(AtomicBool::new(false)),
        };
        let ran = match command.request.clone() {
            Some(Request::Bootstrap(request)) => bootstrap(caller, request, &forward).await,
            Some(Request::Repair(_)) => repair(caller, &forward).await,
            Some(Request::Install(_) | Request::Remove(_) | Request::Doctor(_)) | None => {
                Err(Box::new(unserved(&command)))
            }
        };
        if let Err(fault) = ran
            && !forward.started.load(Ordering::SeqCst)
        {
            crate::root::fail(command, *fault, &mut forward);
        }
    }
}

pub async fn run() -> ExitCode {
    if !mix_shell::effect::accounts::is_root() {
        mix_ui::report(
            mix_ui::Severity::Error,
            &mix_ui::Report::new(&mix_ui::phrase!(
                "the worker must be started by `mix` itself, as root"
            )),
        );
        return ExitCode::FAILURE;
    }
    match mix_rpc::serve_stdin(CliWorker).await {
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
