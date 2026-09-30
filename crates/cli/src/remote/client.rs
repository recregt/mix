use futures_util::{Stream, StreamExt};
use mix_events::v1::Envelope;
use mix_rpc::{
    BootstrapRequest, Client, Event, Failure, Level, Malformed, Mirror, Outcome, RepairRequest,
};
use mix_shell::ops::repair::Repair;
use mix_shell::render::Render;
use mix_shell::target::Error as TargetError;
use prost::Message;

use super::convert::{bootstrap_error_from, report_from_wire, target_error_from};
use crate::render::human::{Exit, Human, Reporters};

const LAUNCHER: &str = "sudo";
const WORKER: &str = "worker";

pub fn level_for(verbosity: u8) -> Level {
    match verbosity {
        0 => Level::Warn,
        1 => Level::Info,
        2 => Level::Debug,
        _ => Level::Trace,
    }
}

struct Replay {
    human: Human,
}

impl Replay {
    fn new(exit: &Exit) -> Self {
        let mix_ui::Reporters {
            activity,
            steps,
            downloads,
            ..
        } = mix_ui::reporters();
        Self {
            human: Human::new(Reporters {
                downloads,
                steps,
                activity,
            })
            .exit_to(exit),
        }
    }

    fn apply(&mut self, event: Event) -> Result<Option<Outcome>, mix_rpc::Error> {
        match event {
            Event::Envelope(bytes) => {
                let envelope = Envelope::decode(bytes.as_slice()).map_err(|error| {
                    mix_rpc::Error::Malformed(Malformed(format!("an event envelope: {error}")))
                })?;
                self.human.envelope(envelope);
            }
            Event::Log {
                level,
                node,
                message,
            } => {
                let parent = self.human.span(node).and_then(|span| span.id());
                match level {
                    Level::Error => tracing::error!(parent: parent, "{message}"),
                    Level::Warn => tracing::warn!(parent: parent, "{message}"),
                    Level::Info => tracing::info!(parent: parent, "{message}"),
                    Level::Debug => tracing::debug!(parent: parent, "{message}"),
                    Level::Trace => tracing::trace!(parent: parent, "{message}"),
                }
            }
            Event::Finished(outcome) => return Ok(Some(outcome)),
        }
        Ok(None)
    }
}

async fn replay(
    events: impl Stream<Item = Result<Event, mix_rpc::Error>>,
    exit: &Exit,
) -> Result<Outcome, mix_rpc::Error> {
    let interrupts = tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    let mut replay = Replay::new(exit);
    let mut events = std::pin::pin!(events);
    let mut outcome = None;
    while let Some(event) = events.next().await {
        if let Some(finished) = replay.apply(event?)? {
            outcome = Some(finished);
        }
    }
    interrupts.abort();
    outcome.ok_or(mix_rpc::Error::Ended)
}

async fn start() -> anyhow::Result<Client> {
    mix_ui::info("Root required. Re-running with sudo...");
    let program = std::env::current_exe()?;
    Ok(Client::start(&program, &[WORKER], Some(LAUNCHER)).await?)
}

pub async fn bootstrap(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    verbosity: u8,
    exit: &Exit,
) -> anyhow::Result<()> {
    let mut client = start().await?;
    let request = BootstrapRequest {
        mirror: mirror.map(|url| Mirror {
            url: url.to_string(),
            key: mirror_key.map(str::to_string),
        }),
        force,
        log_level: level_for(verbosity),
    };
    let outcome = replay(client.bootstrap(&request).await?, exit).await?;
    let _ = client.wait().await;
    match outcome {
        Outcome::BootstrapDone => Ok(()),
        Outcome::Failure(failure) => Err(bootstrap_error_from(failure).into()),
        Outcome::RepairDone { .. } => Err(mix_rpc::Error::Ended.into()),
    }
}

pub async fn repair(verbosity: u8, exit: &Exit) -> anyhow::Result<Repair> {
    let mut client = start().await?;
    let request = RepairRequest {
        log_level: level_for(verbosity),
    };
    let outcome = replay(client.repair(&request).await?, exit).await?;
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
