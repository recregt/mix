use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use mix_events::v1::{Level, Log};
use mix_events::{NodeId, Outbox};
use tracing::field::{Field, Visit};
use tracing::{Span, Subscriber};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, Filter, Layer};
use tracing_subscriber::registry::LookupSpan;

pub const TARGET: &str = "mix_shell::logs";

const OWN_TARGETS: &str = "mix_";

struct Anchor {
    outbox: Arc<Outbox>,
    node: NodeId,
    level: tracing::Level,
}

pub(crate) fn anchor(
    parent: Option<&Span>,
    outbox: &Arc<Outbox>,
    node: NodeId,
    level: tracing::Level,
) -> Span {
    let span = match parent {
        Some(parent) => tracing::info_span!(target: TARGET, parent: parent, "node", node),
        None => tracing::info_span!(target: TARGET, "node", node),
    };
    span.with_subscriber(|(id, dispatch)| {
        if let Some(registry) = dispatch.downcast_ref::<Registry>()
            && let Some(data) = registry.span(id)
        {
            data.extensions_mut().insert(Anchor {
                outbox: Arc::clone(outbox),
                node,
                level,
            });
        }
    });
    span
}

pub fn layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    Logs.with_filter(targets())
}

fn targets<S>() -> impl Filter<S> {
    Targets::new()
        .with_target(OWN_TARGETS, LevelFilter::TRACE)
        .with_default(LevelFilter::WARN)
}

struct Logs;

impl<S> Layer<S> for Logs
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let Some(scope) = ctx.event_scope(event) else {
            return;
        };
        let metadata = event.metadata();
        for span in scope {
            let extensions = span.extensions();
            let Some(anchor) = extensions.get::<Anchor>() else {
                continue;
            };
            if *metadata.level() > anchor.level {
                return;
            }
            let mut fields = Fields::default();
            event.record(&mut fields);
            anchor.outbox.log(Log {
                level: level_of(metadata.level()) as i32,
                target: metadata.target().to_string(),
                message: fields.message,
                fields: fields.rest,
                node: anchor.node,
            });
            return;
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

#[derive(Default)]
struct Fields {
    message: String,
    rest: HashMap<String, String>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.rest
                .insert(field.name().to_string(), value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            self.rest
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use mix_events::v1::Command;
    use mix_events::v1::envelope::Event;
    use mix_events::{ROOT, Start, Tree};
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    fn running() -> (Arc<Outbox>, Tree) {
        let outbox = Arc::new(Outbox::new("request", || {}));
        let tree = Tree::new(
            Arc::clone(&outbox),
            Arc::new(|| None),
            Start::command("doctor", Command::default()),
        );
        (outbox, tree)
    }

    fn logs_in(outbox: &Outbox) -> Vec<Log> {
        outbox
            .drain()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Some(Event::Log(log)) => Some(log),
                _ => None,
            })
            .collect()
    }

    fn logged(level: tracing::Level, emit: impl FnOnce()) -> Vec<Log> {
        let (outbox, _tree) = running();
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            anchor(None, &outbox, ROOT, level).in_scope(emit);
        });
        logs_in(&outbox)
    }

    #[test]
    fn a_log_carries_its_node_target_and_every_field() {
        let logs = logged(tracing::Level::DEBUG, || {
            tracing::debug!(target: "mix_shell::nix", path = "/nix/store/x", attempt = 2, "building");
        });

        assert_eq!(logs.len(), 1);
        let log = &logs[0];
        assert_eq!(log.node, ROOT);
        assert_eq!(log.level, Level::Debug as i32);
        assert_eq!(log.target, "mix_shell::nix");
        assert_eq!(log.message, "building");
        assert_eq!(log.fields["path"], "/nix/store/x");
        assert_eq!(log.fields["attempt"], "2");
    }

    #[test]
    fn a_log_above_the_anchors_level_is_left_out() {
        let logs = logged(tracing::Level::INFO, || {
            tracing::info!(target: "mix_shell::nix", "kept");
            tracing::debug!(target: "mix_shell::nix", "left out");
        });

        let messages: Vec<_> = logs.iter().map(|log| log.message.as_str()).collect();
        assert_eq!(messages, ["kept"]);
    }

    #[test]
    fn another_crate_is_heard_only_from_its_warnings() {
        let logs = logged(tracing::Level::TRACE, || {
            tracing::info!(target: "hyper::client", "connected");
            tracing::warn!(target: "hyper::client", "reset");
        });

        let messages: Vec<_> = logs.iter().map(|log| log.message.as_str()).collect();
        assert_eq!(messages, ["reset"]);
    }

    #[test]
    fn a_log_outside_any_node_is_not_captured() {
        let (outbox, _tree) = running();
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            let _span = anchor(None, &outbox, ROOT, tracing::Level::TRACE);
            tracing::warn!(target: "mix_shell::nix", "elsewhere");
        });

        assert!(logs_in(&outbox).is_empty());
    }

    #[test]
    fn a_nested_span_still_finds_its_nodes_anchor() {
        let logs = logged(tracing::Level::INFO, || {
            tracing::info_span!("inner").in_scope(|| tracing::info!(target: "mix_exec", "ran"));
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].node, ROOT);
    }
}
