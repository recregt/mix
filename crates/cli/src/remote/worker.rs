use std::collections::HashMap;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};

use mix_core::paths::LOCK_FILE;
use mix_events::NodeId;
use mix_events::v1::{Envelope, envelope};
use mix_rpc::{BootstrapRequest, Caller, Event, Events, Failure, Level, Outcome, RepairRequest};
use mix_shell::render::Render;
use prost::Message;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use crate::controls;

use super::convert::{failure_from_bootstrap, report_to_wire};

const NODE_SPAN: &str = "node";
const OWN_TARGETS: &str = "mix_";

#[derive(Default)]
struct Forwarding {
    events: Option<Events>,
    level: Option<Level>,
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
    node: Option<u64>,
    rest: Vec<(String, String)>,
}

impl Visit for Fields {
    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "node" {
            self.node = Some(value);
        } else {
            self.rest
                .push((field.name().to_string(), value.to_string()));
        }
    }

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

struct Node(u64);

struct Logs(Shared);

impl<S> Layer<S> for Logs
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if attrs.metadata().name() != NODE_SPAN {
            return;
        }
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        if let (Some(node), Some(span)) = (fields.node, ctx.span(id)) {
            span.extensions_mut().insert(Node(node));
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        if !metadata.target().starts_with(OWN_TARGETS) {
            return;
        }
        let level = level_of(metadata.level());
        let wanted = self.0.with(|forwarding| {
            forwarding.events.is_some() && forwarding.level.is_some_and(|max| level <= max)
        });
        if !wanted {
            return;
        }
        let node = ctx
            .event_scope(event)
            .and_then(|scope| {
                scope
                    .into_iter()
                    .find_map(|span| span.extensions().get::<Node>().map(|node| node.0))
            })
            .unwrap_or_default();
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut message = fields.message;
        for (name, value) in fields.rest {
            let _ = write!(message, " {name}={value}");
        }
        self.0.send(Event::Log {
            level,
            node,
            message,
        });
    }
}

struct Forward {
    events: Events,
    spans: HashMap<NodeId, tracing::Span>,
}

impl Forward {
    fn new(events: Events) -> Self {
        Self {
            events,
            spans: HashMap::new(),
        }
    }
}

impl Render for Forward {
    fn envelope(&mut self, envelope: Envelope) {
        match &envelope.event {
            Some(envelope::Event::NodeStarted(started)) => {
                let node = started.id;
                self.spans
                    .insert(node, tracing::info_span!(NODE_SPAN, node));
            }
            Some(envelope::Event::NodeFinished(finished)) => {
                self.spans.remove(&finished.id);
            }
            _ => {}
        }
        let _ = self.events.send(Event::Envelope(envelope.encode_to_vec()));
    }

    fn span(&self, node: NodeId) -> Option<tracing::Span> {
        self.spans.get(&node).cloned()
    }
}

fn client_gone(events: &Events) -> impl Future<Output = ()> + Send + 'static {
    let events = events.clone();
    async move { events.closed().await }
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
        let outcome = match mix_shell::effect::lock::acquire_exclusive(LOCK_FILE) {
            Err(error) => Outcome::Failure(Failure::Core(error)),
            Ok(_lock) => {
                let mirror = request.mirror.as_ref();
                let policy = match crate::commands::requested_policy(
                    mirror.map(|mirror| mirror.url.as_str()),
                    mirror.and_then(|mirror| mirror.key.as_deref()),
                ) {
                    Ok(policy) => policy,
                    Err(error) => {
                        self.0.stop();
                        return Outcome::Failure(failure_from_bootstrap(error));
                    }
                };
                let ctx = mix_shell::Context::new(mix_exec::Scope::root())
                    .with_user(
                        mix_shell::effect::accounts::user_by_uid(caller.uid)
                            .and_then(mix_shell::profile::user_config_for),
                    )
                    .with_render(Forward::new(events.clone()))
                    .with_policy(policy)
                    .with_host(crate::commands::host_config());
                let _watch = controls::watch(
                    &ctx.scope,
                    controls::BOOTSTRAP,
                    client_gone(&events),
                    controls::Side::Worker,
                );
                let result = mix_shell::ops::bootstrap::bootstrap(&ctx, request.force).await;
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
        self.0.start(events.clone(), request.log_level);
        let outcome = match mix_shell::effect::lock::acquire_exclusive(LOCK_FILE) {
            Err(error) => Outcome::Failure(Failure::Core(error)),
            Ok(_lock) => {
                let ctx = mix_shell::Context::new(mix_exec::Scope::root())
                    .with_user(
                        mix_shell::effect::accounts::user_by_uid(caller.uid)
                            .and_then(mix_shell::profile::existing_user_config_for),
                    )
                    .with_render(Forward::new(events.clone()))
                    .with_policy(crate::commands::policy())
                    .with_host(crate::commands::host_config());
                let _watch = controls::watch(
                    &ctx.scope,
                    controls::REPAIR,
                    client_gone(&events),
                    controls::Side::Worker,
                );
                let repair = mix_shell::ops::repair::repair(&ctx).await;
                Outcome::RepairDone {
                    reports: repair.reports.into_iter().map(report_to_wire).collect(),
                    interrupted: repair.interrupted,
                }
            }
        };
        self.0.stop();
        outcome
    }
}

pub async fn run() -> ExitCode {
    let shared = Shared::default();
    if tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(Logs(shared.clone())),
    )
    .is_err()
    {
        return ExitCode::FAILURE;
    }
    if !mix_shell::effect::accounts::is_root() {
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
