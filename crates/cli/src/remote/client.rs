use std::fmt::Write as _;

use futures_util::{Stream, StreamExt};
use mix_events::v1::{Envelope, Level as LogLevel, Log, envelope};
use mix_rpc::{
    BootstrapRequest, Client, Event, Failure, Level, Malformed, Mirror, Outcome, RepairRequest,
};
use mix_shell::ops::repair::Repair;
use mix_shell::render::Render;
use mix_shell::target::Error as TargetError;
use prost::Message;

use super::convert::{bootstrap_error_from, report_from_wire, target_error_from};
use crate::cli::Output;
use crate::render::human::Reporters;
use crate::render::sinks::{Sinks, View};

const LAUNCHER: &str = "sudo";
const WORKER: &str = "worker";

fn level_for(view: &View) -> Level {
    match view.logs() {
        tracing::Level::ERROR => Level::Error,
        tracing::Level::WARN => Level::Warn,
        tracing::Level::INFO => Level::Info,
        tracing::Level::DEBUG => Level::Debug,
        tracing::Level::TRACE => Level::Trace,
    }
}

struct Replay {
    sinks: Sinks,
    terminal: bool,
}

impl Replay {
    fn new(view: &View) -> Result<Self, mix_rpc::Error> {
        let mix_ui::Reporters {
            activity,
            steps,
            downloads,
            ..
        } = mix_ui::reporters();
        let sinks = view
            .sinks(Reporters {
                downloads,
                steps,
                activity,
            })
            .map_err(mix_rpc::Error::Spawn)?;
        Ok(Self {
            sinks,
            terminal: view.output == Output::Human,
        })
    }

    fn apply(&mut self, event: Event) -> Result<Option<Outcome>, mix_rpc::Error> {
        match event {
            Event::Envelope(bytes) => {
                let envelope = Envelope::decode(bytes.as_slice()).map_err(|error| {
                    mix_rpc::Error::Malformed(Malformed(format!("an event envelope: {error}")))
                })?;
                if self.terminal
                    && let Some(envelope::Event::Log(log)) = &envelope.event
                {
                    self.print(log);
                }
                self.sinks.envelope(envelope);
            }
            Event::Finished(outcome) => return Ok(Some(outcome)),
        }
        Ok(None)
    }
}

impl Replay {
    fn print(&self, log: &Log) {
        let parent = self.sinks.span(log.node).and_then(|span| span.id());
        let mut fields: Vec<_> = log.fields.iter().collect();
        fields.sort_unstable();
        let mut message = log.message.clone();
        for (name, value) in fields {
            let _ = write!(message, " {name}={value}");
        }
        match log.level() {
            LogLevel::Error => tracing::error!(parent: parent, "{message}"),
            LogLevel::Warn | LogLevel::Unspecified => tracing::warn!(parent: parent, "{message}"),
            LogLevel::Info => tracing::info!(parent: parent, "{message}"),
            LogLevel::Debug => tracing::debug!(parent: parent, "{message}"),
            LogLevel::Trace => tracing::trace!(parent: parent, "{message}"),
        }
    }
}

async fn replay(
    events: impl Stream<Item = Result<Event, mix_rpc::Error>>,
    view: &View,
) -> Result<Outcome, mix_rpc::Error> {
    let interrupts = tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    let mut replay = Replay::new(view)?;
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

async fn start(view: &View) -> anyhow::Result<Client> {
    if view.output == Output::Human {
        mix_ui::info("Root required. Re-running with sudo...");
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
    let request = BootstrapRequest {
        mirror: mirror.map(|url| Mirror {
            url: url.to_string(),
            key: mirror_key.map(str::to_string),
        }),
        force,
        log_level: level_for(view),
    };
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
    let request = RepairRequest {
        log_level: level_for(view),
    };
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
