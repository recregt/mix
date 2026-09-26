use std::collections::HashSet;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};

use mix_core::paths::LOCK_FILE;
use mix_core::{ActivityReporter, BuildProgress, DownloadProgress, StepObserver};
use mix_rpc::{BootstrapRequest, Caller, Event, Events, Failure, Level, Outcome, RepairRequest};
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use super::convert::{failure_from_bootstrap, report_to_wire};

const FORWARDED_SPANS: [&str; 2] = ["step", "rollback"];
const OWN_TARGETS: &str = "mix_";

#[derive(Default)]
struct Forwarding {
    events: Option<Events>,
    level: Option<Level>,
    open: HashSet<u64>,
    failed: HashSet<u64>,
}

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Forwarding>>);

impl Shared {
    fn with<T>(&self, f: impl FnOnce(&mut Forwarding) -> T) -> T {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn start(&self, events: Events, level: Level) {
        self.with(|forwarding| {
            *forwarding = Forwarding {
                events: Some(events),
                level: Some(level),
                ..Forwarding::default()
            }
        });
    }

    fn stop(&self) {
        self.with(|forwarding| *forwarding = Forwarding::default());
    }

    fn send(&self, event: Event) {
        self.with(|forwarding| {
            if let Some(events) = &forwarding.events {
                let _ = events.send(event);
            }
        });
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    rest: Vec<(String, String)>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.rest
                .push((field.name().to_string(), value.to_string()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            self.rest
                .push((field.name().to_string(), format!("{value:?}")));
        }
    }
}

fn level_of(level: &tracing::Level) -> Level {
    match *level {
        tracing::Level::ERROR => Level::Error,
        tracing::Level::WARN => Level::Warn,
        tracing::Level::INFO => Level::Info,
        tracing::Level::DEBUG => Level::Debug,
        tracing::Level::TRACE => Level::Trace,
    }
}

struct Forwarder(Shared);

impl<S> Layer<S> for Forwarder
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let name = attrs.metadata().name();
        if !FORWARDED_SPANS.contains(&name) {
            return;
        }
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let parent = attrs
            .parent()
            .cloned()
            .or_else(|| {
                attrs
                    .is_contextual()
                    .then(|| ctx.current_span().id().cloned())
                    .flatten()
            })
            .map(|parent| parent.into_u64());
        let id = id.into_u64();

        let opened = self.0.with(|forwarding| {
            forwarding.events.as_ref()?;
            forwarding.open.insert(id);
            Some(parent.filter(|parent| forwarding.open.contains(parent)))
        });
        if let Some(parent) = opened {
            self.0.send(Event::SpanOpened {
                id,
                parent,
                name: name.to_string(),
                fields: fields.rest,
            });
        }
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        let id = id.into_u64();
        let closed = self.0.with(|forwarding| {
            forwarding
                .open
                .remove(&id)
                .then(|| forwarding.failed.remove(&id))
        });
        if let Some(failed) = closed {
            self.0.send(Event::SpanClosed { id, failed });
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        if !metadata.target().starts_with(OWN_TARGETS) {
            return;
        }
        let level = level_of(metadata.level());
        let span = self.0.with(|forwarding| {
            let wanted = forwarding.level.is_some_and(|max| level <= max);
            wanted.then(|| {
                ctx.event_scope(event).and_then(|scope| {
                    scope
                        .map(|span| span.id().into_u64())
                        .find(|id| forwarding.open.contains(id))
                })
            })
        });
        let Some(span) = span else {
            return;
        };
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut message = fields.message;
        for (name, value) in fields.rest {
            let _ = write!(message, " {name}={value}");
        }
        self.0.send(Event::Log {
            level,
            span,
            message,
        });
    }
}

struct Downloads(Events);

impl DownloadProgress for Downloads {
    fn set_total(&self, total: u64) {
        let _ = self.0.send(Event::DownloadStarted { total });
    }

    fn add(&self, delta: u64) {
        let _ = self.0.send(Event::DownloadAdvanced { delta });
    }
}

struct Steps(Shared);

impl StepObserver for Steps {
    fn on_step_span(&self, _span: &tracing::Span) {}

    fn on_step_failed(&self, span: &tracing::Span) {
        if let Some(id) = span.id() {
            self.0
                .with(|forwarding| forwarding.failed.insert(id.into_u64()));
        }
    }
}

struct Activity(Events);

impl ActivityReporter for Activity {
    fn line(&self, line: &str) {
        let _ = self.0.send(Event::ActivityLine(line.to_string()));
    }

    fn progress(&self, progress: &BuildProgress) {
        let _ = self.0.send(Event::ActivityProgress(*progress));
    }

    fn clear(&self) {
        let _ = self.0.send(Event::ActivityCleared);
    }
}

struct CliWorker(Shared);

impl mix_rpc::Worker for CliWorker {
    async fn bootstrap(
        &self,
        caller: Caller,
        request: BootstrapRequest,
        events: Events,
    ) -> Outcome {
        self.0.start(events.clone(), request.log_level);
        let outcome = match mix_core::lock::acquire_exclusive(LOCK_FILE) {
            Err(error) => Outcome::Failure(Failure::Core(error)),
            Ok(_lock) => {
                let mirror = request.mirror.as_ref();
                let result = mix_app::bootstrap::bootstrap(
                    mix_core::privilege::user_by_uid(caller.uid),
                    mirror.map(|mirror| mirror.url.as_str()),
                    mirror.and_then(|mirror| mirror.key.as_deref()),
                    request.force,
                    Arc::new(Downloads(events.clone())),
                    Arc::new(Steps(self.0.clone())),
                    Arc::new(Activity(events.clone())),
                )
                .await;
                match result {
                    Ok(_) => Outcome::BootstrapDone,
                    Err(error) => Outcome::Failure(failure_from_bootstrap(error)),
                }
            }
        };
        self.0.stop();
        outcome
    }

    async fn repair(&self, caller: Caller, request: RepairRequest, events: Events) -> Outcome {
        self.0.start(events, request.log_level);
        let outcome = match mix_core::lock::acquire_exclusive(LOCK_FILE) {
            Err(error) => Outcome::Failure(Failure::Core(error)),
            Ok(_lock) => {
                let user_config = mix_core::privilege::user_by_uid(caller.uid)
                    .and_then(mix_app::profile::existing_user_config_for);
                let reports = mix_app::repair::repair(user_config.as_ref()).await;
                Outcome::RepairDone(reports.into_iter().map(report_to_wire).collect())
            }
        };
        self.0.stop();
        outcome
    }
}

pub async fn run() -> ExitCode {
    let shared = Shared::default();
    if tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(Forwarder(shared.clone())),
    )
    .is_err()
    {
        return ExitCode::FAILURE;
    }
    if !mix_core::privilege::is_root() {
        eprintln!("mix worker must be started by mix itself, as root");
        return ExitCode::FAILURE;
    }
    match mix_rpc::serve_stdin(CliWorker(shared)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
