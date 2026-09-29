use std::collections::HashMap;
use std::sync::Arc;

use mix_events::v1::{
    Envelope, NodeProgress, Status, envelope::Event, node_progress, node_started,
};
use mix_events::{NodeId, Outbox};

use crate::Reporters;
use crate::drive::Observer;

pub struct Bridge {
    outbox: Arc<Outbox>,
    reporters: Reporters,
    spans: HashMap<NodeId, tracing::Span>,
    titles: HashMap<NodeId, String>,
    received: HashMap<NodeId, u64>,
}

impl Bridge {
    pub fn new(outbox: Arc<Outbox>, reporters: Reporters) -> Self {
        Self {
            outbox,
            reporters,
            spans: HashMap::new(),
            titles: HashMap::new(),
            received: HashMap::new(),
        }
    }

    fn replay(&mut self, envelope: Envelope) {
        let Some(event) = envelope.event else {
            return;
        };
        match event {
            Event::NodeStarted(node) => match node.kind {
                Some(node_started::Kind::Step(step)) => {
                    tracing::info!("running: {}", step.title);
                    let span = tracing::info_span!("step", name = step.title.as_str());
                    self.reporters.steps.on_step_span(&span);
                    self.titles.insert(node.id, step.title);
                    self.spans.insert(node.id, span);
                }
                Some(node_started::Kind::Rollback(rollback)) => {
                    let title = self
                        .titles
                        .get(&rollback.undoes)
                        .cloned()
                        .unwrap_or_default();
                    tracing::info!("rolling back: {title}");
                    let span = tracing::info_span!("rollback", name = title.as_str());
                    self.reporters.steps.on_step_span(&span);
                    self.spans.insert(node.id, span);
                }
                _ => {}
            },
            Event::NodeFinished(node) => {
                self.received.remove(&node.id);
                let failed = node.status() == Status::Failed;
                if failed && let Some(span) = self.spans.get(&node.id) {
                    let message = node
                        .diagnostic
                        .as_ref()
                        .map(|diagnostic| diagnostic.message.clone())
                        .unwrap_or_default();
                    span.in_scope(|| match self.titles.get(&node.id) {
                        Some(title) => tracing::debug!("step failed: {title} ({message})"),
                        None => tracing::error!("rollback failed: {message}"),
                    });
                }
                if let Some(span) = self.spans.remove(&node.id) {
                    self.reporters
                        .steps
                        .on_step_closed(&span, node.status() == Status::Failed);
                }
            }
            Event::NodeProgress(NodeProgress {
                id,
                progress: Some(node_progress::Progress::Bytes(bytes)),
            }) => {
                let Some(span) = self.spans.get(&id) else {
                    return;
                };
                let first = !self.received.contains_key(&id);
                let before = self.received.insert(id, bytes.done).unwrap_or(0);
                let downloads = Arc::clone(&self.reporters.downloads);
                span.in_scope(|| {
                    if first && let Some(total) = bytes.total {
                        downloads.set_total(total);
                    }
                    downloads.add(bytes.done.saturating_sub(before));
                });
            }
            Event::Diagnostic(diagnostic) => tracing::warn!("{}", diagnostic.message),
            _ => {}
        }
    }
}

impl Observer for Bridge {
    fn flush(&mut self) {
        for envelope in self.outbox.drain() {
            self.replay(envelope);
        }
    }

    fn span(&self, node: NodeId) -> Option<tracing::Span> {
        self.spans.get(&node).cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mix_core::{DownloadProgress, StepObserver};
    use mix_events::v1::{Bytes, Command, Step};
    use mix_events::{Ending, ROOT, Start, Tree};

    use super::*;

    #[derive(Default)]
    struct Seen(Mutex<Vec<String>>);

    impl StepObserver for Seen {
        fn on_step_span(&self, _span: &tracing::Span) {
            self.0.lock().unwrap().push("opened".into());
        }

        fn on_step_closed(&self, _span: &tracing::Span, failed: bool) {
            self.0
                .lock()
                .unwrap()
                .push(format!("closed failed={failed}"));
        }
    }

    impl DownloadProgress for Seen {
        fn set_total(&self, total: u64) {
            self.0.lock().unwrap().push(format!("total {total}"));
        }

        fn add(&self, delta: u64) {
            self.0.lock().unwrap().push(format!("add {delta}"));
        }
    }

    #[test]
    fn a_step_and_its_download_reach_the_display_as_before() {
        let seen = Arc::new(Seen::default());
        let outbox = Arc::new(Outbox::new("request", || {}));
        let mut bridge = Bridge::new(
            outbox.clone(),
            Reporters {
                downloads: seen.clone(),
                steps: seen.clone(),
                activity: Arc::new(mix_core::NoopActivity),
            },
        );
        let mut tree = Tree::new(
            outbox,
            Arc::new(|| None),
            Start::command("bootstrap", Command::default()),
        );
        let step = tree
            .start(
                ROOT,
                Start::new(
                    "fetch-runtime",
                    node_started::Kind::Step(Step {
                        title: "fetch the runtime".into(),
                    }),
                ),
            )
            .unwrap();
        for done in [4, 10] {
            tree.progress(
                step,
                node_progress::Progress::Bytes(Bytes {
                    done,
                    total: Some(10),
                }),
            )
            .unwrap();
            bridge.flush();
        }
        tree.finish(step, Ending::succeeded()).unwrap();
        bridge.flush();

        assert_eq!(
            *seen.0.lock().unwrap(),
            [
                "opened",
                "total 10",
                "add 4",
                "add 6",
                "closed failed=false"
            ]
        );
    }
}
