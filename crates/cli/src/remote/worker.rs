use std::process::ExitCode;

use mix_core::paths::LOCK_FILE;
use mix_events::v1::Envelope;
use mix_rpc::{BootstrapRequest, Caller, Event, Events, Failure, Outcome, RepairRequest};
use mix_shell::render::Render;
use prost::Message;

use crate::controls;

use super::convert::{failure_from_bootstrap, report_to_wire};

struct Forward(Events);

impl Render for Forward {
    fn envelope(&mut self, envelope: Envelope) {
        let _ = self.0.send(Event::Envelope(envelope.encode_to_vec()));
    }

    fn detail(&self) -> mix_events::Detail {
        mix_events::Detail::Trace
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
                    .with_render(Forward(events.clone()))
                    .with_policy(policy)
                    .with_host(crate::commands::host_config());
                let _watch = controls::watch(
                    &ctx.scope,
                    None,
                    client_gone(&events),
                    controls::Side::Worker,
                );
                let result = mix_shell::ops::bootstrap::bootstrap(&ctx, request.force).await;
                match result {
                    Ok(_) => Outcome::BootstrapDone,
                    Err(error) => Outcome::Failure(failure_from_bootstrap(error)),
                }
            }
        }
    }

    async fn repair(&self, caller: Caller, _request: RepairRequest, events: Events) -> Outcome {
        match mix_shell::effect::lock::acquire_exclusive(LOCK_FILE) {
            Err(error) => Outcome::Failure(Failure::Core(error)),
            Ok(_lock) => {
                let ctx = mix_shell::Context::new(mix_exec::Scope::root())
                    .with_user(
                        mix_shell::effect::accounts::user_by_uid(caller.uid)
                            .and_then(mix_shell::profile::existing_user_config_for),
                    )
                    .with_render(Forward(events.clone()))
                    .with_policy(crate::commands::policy())
                    .with_host(crate::commands::host_config());
                let _watch = controls::watch(
                    &ctx.scope,
                    None,
                    client_gone(&events),
                    controls::Side::Worker,
                );
                let repair = mix_shell::ops::repair::repair(&ctx).await;
                Outcome::RepairDone {
                    reports: repair.reports.into_iter().map(report_to_wire).collect(),
                    interrupted: repair.interrupted,
                }
            }
        }
    }
}

pub async fn run() -> ExitCode {
    if !mix_shell::effect::accounts::is_root() {
        mix_ui::report(
            mix_ui::Severity::Error,
            &mix_ui::Report {
                summary: "the worker must be started by `mix` itself, as root",
                ..mix_ui::Report::default()
            },
        );
        return ExitCode::FAILURE;
    }
    match mix_rpc::serve_stdin(CliWorker).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            mix_ui::report(
                mix_ui::Severity::Error,
                &mix_ui::Report {
                    summary: "the worker stopped serving its client",
                    causes: vec![error.to_string()],
                    ..mix_ui::Report::default()
                },
            );
            ExitCode::FAILURE
        }
    }
}
