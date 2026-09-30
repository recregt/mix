use std::process::ExitCode;

use mix_core::paths::LOCK_FILE;
use mix_events::v1::Envelope;
use mix_rpc::{BootstrapRequest, Caller, Event, Events, Failure, Level, Outcome, RepairRequest};
use mix_shell::render::Render;
use prost::Message;
use tracing::Instrument;
use tracing_subscriber::layer::SubscriberExt;

use crate::controls;

use super::convert::{failure_from_bootstrap, report_to_wire};

struct Forward {
    events: Events,
    logs: tracing::Level,
}

impl Forward {
    fn new(events: Events, logs: Level) -> Self {
        let logs = match logs {
            Level::Error => tracing::Level::ERROR,
            Level::Warn => tracing::Level::WARN,
            Level::Info => tracing::Level::INFO,
            Level::Debug => tracing::Level::DEBUG,
            Level::Trace => tracing::Level::TRACE,
        };
        Self { events, logs }
    }
}

impl Render for Forward {
    fn envelope(&mut self, envelope: Envelope) {
        let _ = self.events.send(Event::Envelope(envelope.encode_to_vec()));
    }

    fn logs(&self) -> Option<tracing::Level> {
        Some(self.logs)
    }
}

fn client_gone(events: &Events) -> impl Future<Output = ()> + Send + 'static {
    let events = events.clone();
    async move { events.closed().await }
}

struct CliWorker;

impl mix_rpc::Worker for CliWorker {
    async fn bootstrap(
        &self,
        caller: Caller,
        request: BootstrapRequest,
        events: Events,
    ) -> Outcome {
        match mix_shell::effect::lock::acquire_exclusive(LOCK_FILE) {
            Err(error) => Outcome::Failure(Failure::Core(error)),
            Ok(_lock) => {
                let mirror = request.mirror.as_ref();
                let policy = match crate::commands::requested_policy(
                    mirror.map(|mirror| mirror.url.as_str()),
                    mirror.and_then(|mirror| mirror.key.as_deref()),
                ) {
                    Ok(policy) => policy,
                    Err(error) => {
                        return Outcome::Failure(failure_from_bootstrap(error));
                    }
                };
                let ctx = mix_shell::Context::new(mix_exec::Scope::root())
                    .with_user(
                        mix_shell::effect::accounts::user_by_uid(caller.uid)
                            .and_then(mix_shell::profile::user_config_for),
                    )
                    .with_render(Forward::new(events.clone(), request.log_level))
                    .with_policy(policy)
                    .with_host(crate::commands::host_config());
                let _watch = controls::watch(
                    &ctx,
                    controls::BOOTSTRAP,
                    client_gone(&events),
                    controls::Side::Worker,
                );
                let result = mix_shell::ops::bootstrap::bootstrap(&ctx, request.force)
                    .instrument(ctx.span())
                    .await;
                match result {
                    Ok(_) => Outcome::BootstrapDone,
                    Err(error) => Outcome::Failure(failure_from_bootstrap(error)),
                }
            }
        }
    }

    async fn repair(&self, caller: Caller, request: RepairRequest, events: Events) -> Outcome {
        match mix_shell::effect::lock::acquire_exclusive(LOCK_FILE) {
            Err(error) => Outcome::Failure(Failure::Core(error)),
            Ok(_lock) => {
                let ctx = mix_shell::Context::new(mix_exec::Scope::root())
                    .with_user(
                        mix_shell::effect::accounts::user_by_uid(caller.uid)
                            .and_then(mix_shell::profile::existing_user_config_for),
                    )
                    .with_render(Forward::new(events.clone(), request.log_level))
                    .with_policy(crate::commands::policy())
                    .with_host(crate::commands::host_config());
                let _watch = controls::watch(
                    &ctx,
                    controls::REPAIR,
                    client_gone(&events),
                    controls::Side::Worker,
                );
                let repair = mix_shell::ops::repair::repair(&ctx)
                    .instrument(ctx.span())
                    .await;
                Outcome::RepairDone {
                    reports: repair.reports.into_iter().map(report_to_wire).collect(),
                    interrupted: repair.interrupted,
                }
            }
        }
    }
}

pub async fn run() -> ExitCode {
    if tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(mix_shell::logs::layer()),
    )
    .is_err()
    {
        return ExitCode::FAILURE;
    }
    if !mix_shell::effect::accounts::is_root() {
        eprintln!("mix worker must be started by mix itself, as root");
        return ExitCode::FAILURE;
    }
    match mix_rpc::serve_stdin(CliWorker).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
