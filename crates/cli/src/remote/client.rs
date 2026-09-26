use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{Stream, StreamExt};
use mix_app::repair::Repair;
use mix_app::target::Error as TargetError;
use mix_core::{ActivityReporter, DownloadProgress, StepObserver};
use mix_rpc::{BootstrapRequest, Client, Event, Failure, Level, Mirror, Outcome, RepairRequest};

use super::convert::{bootstrap_error_from, report_from_wire, target_error_from};

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
    spans: HashMap<u64, tracing::Span>,
    open: Vec<u64>,
    steps: Arc<dyn StepObserver>,
    downloads: Arc<dyn DownloadProgress>,
    activity: Arc<dyn ActivityReporter>,
}

impl Replay {
    fn new() -> Self {
        Self {
            spans: HashMap::new(),
            open: Vec::new(),
            steps: mix_ui::step_observer(),
            downloads: mix_ui::download_reporter(),
            activity: mix_ui::activity_reporter(),
        }
    }

    fn in_current(&self, f: impl FnOnce()) {
        match self.open.last().and_then(|id| self.spans.get(id)) {
            Some(span) => span.in_scope(f),
            None => f(),
        }
    }

    fn apply(&mut self, event: Event) -> Option<Outcome> {
        match event {
            Event::SpanOpened {
                id,
                parent,
                name,
                fields,
            } => {
                let parent = parent.and_then(|parent| self.spans.get(&parent)?.id());
                let label = fields
                    .iter()
                    .find(|(field, _)| field == "name")
                    .map_or("", |(_, value)| value.as_str());
                let span = if name == "rollback" {
                    tracing::info_span!(parent: parent, "rollback", name = label)
                } else {
                    tracing::info_span!(parent: parent, "step", name = label)
                };
                self.steps.on_step_span(&span);
                self.spans.insert(id, span);
                self.open.push(id);
            }
            Event::SpanClosed { id, failed } => {
                if let Some(span) = self.spans.remove(&id)
                    && failed
                {
                    self.steps.on_step_failed(&span);
                }
                self.open.retain(|open| *open != id);
            }
            Event::Log {
                level,
                span,
                message,
            } => {
                let parent = span.and_then(|span| self.spans.get(&span)?.id());
                match level {
                    Level::Error => tracing::error!(parent: parent, "{message}"),
                    Level::Warn => tracing::warn!(parent: parent, "{message}"),
                    Level::Info => tracing::info!(parent: parent, "{message}"),
                    Level::Debug => tracing::debug!(parent: parent, "{message}"),
                    Level::Trace => tracing::trace!(parent: parent, "{message}"),
                }
            }
            Event::DownloadStarted { total } => {
                let downloads = Arc::clone(&self.downloads);
                self.in_current(|| downloads.set_total(total));
            }
            Event::DownloadAdvanced { delta } => {
                let downloads = Arc::clone(&self.downloads);
                self.in_current(|| downloads.add(delta));
            }
            Event::ActivityLine(line) => {
                let activity = Arc::clone(&self.activity);
                self.in_current(|| activity.line(&line));
            }
            Event::ActivityProgress(progress) => {
                let activity = Arc::clone(&self.activity);
                self.in_current(|| activity.progress(&progress));
            }
            Event::ActivityCleared => {
                let activity = Arc::clone(&self.activity);
                self.in_current(|| activity.clear());
            }
            Event::Finished(outcome) => return Some(outcome),
        }
        None
    }
}

async fn replay(
    events: impl Stream<Item = Result<Event, mix_rpc::Error>>,
) -> Result<Outcome, mix_rpc::Error> {
    let interrupts = tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    let mut replay = Replay::new();
    let mut events = std::pin::pin!(events);
    let mut outcome = None;
    while let Some(event) = events.next().await {
        if let Some(finished) = replay.apply(event?) {
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
    let outcome = replay(client.bootstrap(&request).await?).await?;
    let _ = client.wait();
    match outcome {
        Outcome::BootstrapDone => Ok(()),
        Outcome::Failure(failure) => Err(bootstrap_error_from(failure).into()),
        Outcome::RepairDone { .. } => Err(mix_rpc::Error::Ended.into()),
    }
}

pub async fn repair(verbosity: u8) -> anyhow::Result<Repair> {
    let mut client = start().await?;
    let request = RepairRequest {
        log_level: level_for(verbosity),
    };
    let outcome = replay(client.repair(&request).await?).await?;
    let _ = client.wait();
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
