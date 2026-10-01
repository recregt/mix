use futures_util::{Stream, StreamExt};
use mix_events::v1::Envelope;
use mix_events::{Detail, Normalize};
use mix_rpc::{
    BootstrapRequest, Client, Event, Failure, Malformed, Mirror, Outcome, RepairRequest,
};
use mix_shell::ops::repair::Repair;
use mix_shell::render::Render;
use mix_shell::target::Error as TargetError;
use prost::Message;

use super::convert::{bootstrap_error_from, report_from_wire, target_error_from};
use crate::cli::Output;
use crate::render::sinks::{Sinks, View};

const LAUNCHER: &str = "sudo";
const WORKER: &str = "worker";

fn envelope(bytes: &[u8]) -> Result<Envelope, mix_rpc::Error> {
    let mut envelope = Envelope::decode(bytes).map_err(|error| {
        mix_rpc::Error::Malformed(Malformed(format!("an event envelope: {error}")))
    })?;
    envelope.normalize();
    Ok(envelope)
}

async fn replay(
    events: impl Stream<Item = Result<Event, mix_rpc::Error>>,
    view: &View,
) -> Result<Outcome, mix_rpc::Error> {
    let interrupts = tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    let mut sinks: Sinks = view
        .sinks(mix_ui::display())
        .map_err(mix_rpc::Error::Spawn)?;
    let mut events = std::pin::pin!(events);
    let mut outcome = None;
    while let Some(event) = events.next().await {
        match event? {
            Event::Envelope(bytes) => sinks.envelope(envelope(&bytes)?),
            Event::Finished(finished) => outcome = Some(finished),
        }
    }
    interrupts.abort();
    outcome.ok_or(mix_rpc::Error::Ended)
}

fn bootstrap_request(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
) -> BootstrapRequest {
    BootstrapRequest {
        mirror: mirror.map(|url| Mirror {
            url: url.to_string(),
            key: mirror_key.map(str::to_string),
        }),
        force,
    }
}

async fn start(view: &View) -> anyhow::Result<Client> {
    if view.output == Output::Human && view.level() >= Detail::Step {
        mix_ui::note("root is required, re-running with sudo", None);
    }
    let program = std::env::current_exe()?;
    Ok(Client::start(&program, &[WORKER], Some(LAUNCHER)).await?)
}

pub async fn bootstrap(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    view: &View,
) -> anyhow::Result<()> {
    let mut client = start(view).await?;
    let request = bootstrap_request(mirror, mirror_key, force);
    let outcome = replay(client.bootstrap(&request).await?, view).await?;
    let _ = client.wait().await;
    match outcome {
        Outcome::BootstrapDone => Ok(()),
        Outcome::Failure(failure) => Err(bootstrap_error_from(failure).into()),
        Outcome::RepairDone { .. } => Err(mix_rpc::Error::Ended.into()),
    }
}

pub async fn repair(view: &View) -> anyhow::Result<Repair> {
    let mut client = start(view).await?;
    let request = RepairRequest;
    let outcome = replay(client.repair(&request).await?, view).await?;
    let _ = client.wait().await;
    match outcome {
        Outcome::RepairDone {
            reports,
            interrupted,
        } => Ok(Repair {
            reports: reports.into_iter().map(report_from_wire).collect(),
            interrupted,
        }),
        Outcome::Failure(Failure::Core(error)) => Err(TargetError::Core(error).into()),
        Outcome::Failure(Failure::Target(failure)) => Err(target_error_from(failure).into()),
        Outcome::Failure(failure) => Err(bootstrap_error_from(failure).into()),
        Outcome::BootstrapDone => Err(mix_rpc::Error::Ended.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mirror_this_process_resolved_crosses_sudo_in_the_request() {
        let request = bootstrap_request(Some("http://env.internal"), Some("env:KEY"), false);

        assert_eq!(
            request.mirror,
            Some(Mirror {
                url: "http://env.internal".into(),
                key: Some("env:KEY".into()),
            })
        );
        assert_eq!(
            bootstrap_request(None, Some("stray:KEY"), true).mirror,
            None
        );
    }
}
