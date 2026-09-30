use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mix_core::{
    ActivityReporter, BuildProgress, DownloadProgress, NoopActivity, NoopProgress, NoopSteps,
    StepObserver,
};
use mix_events::v1::{
    Envelope, NodeProgress, Status, envelope::Event, node_progress, node_started,
};
use mix_events::{NodeId, ROOT};
use mix_shell::render::Render;

#[derive(Clone)]
pub struct Reporters {
    pub downloads: Arc<dyn DownloadProgress>,
    pub steps: Arc<dyn StepObserver>,
    pub activity: Arc<dyn ActivityReporter>,
}

impl Reporters {
    pub fn silent() -> Self {
        Self {
            downloads: Arc::new(NoopProgress),
            steps: Arc::new(NoopSteps),
            activity: Arc::new(NoopActivity),
        }
    }
}

pub struct Human {
    reporters: Reporters,
    results: bool,
    spans: HashMap<NodeId, tracing::Span>,
    titles: HashMap<NodeId, String>,
    received: HashMap<NodeId, u64>,
    actions: HashSet<NodeId>,
    printed: HashSet<NodeId>,
}

impl Human {
    pub fn new(reporters: Reporters) -> Self {
        Self {
            reporters,
            results: true,
            spans: HashMap::new(),
            titles: HashMap::new(),
            received: HashMap::new(),
            actions: HashSet::new(),
            printed: HashSet::new(),
        }
    }

    pub fn without_results(mut self) -> Self {
        self.results = false;
        self
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
                Some(node_started::Kind::Action(_)) => {
                    if let Some(span) = self.spans.get(&node.parent).cloned() {
                        self.actions.insert(node.id);
                        self.spans.insert(node.id, span);
                    }
                }
                _ => {}
            },
            Event::NodeFinished(node) if node.id == ROOT => {
                if let Some(result) = &node.result {
                    super::results::finished(result, self.results);
                }
            }
            Event::NodeFinished(node) if self.actions.remove(&node.id) => {
                let span = self.spans.remove(&node.id);
                self.received.remove(&node.id);
                if self.printed.remove(&node.id) {
                    let activity = Arc::clone(&self.reporters.activity);
                    match span {
                        Some(span) => span.in_scope(|| activity.clear()),
                        None => activity.clear(),
                    }
                }
            }
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
            Event::NodeProgress(NodeProgress {
                id,
                progress: Some(node_progress::Progress::Line(line)),
            }) => {
                let Some(span) = self.spans.get(&id) else {
                    return;
                };
                self.printed.insert(id);
                let activity = Arc::clone(&self.reporters.activity);
                span.in_scope(|| activity.line(&line.text));
            }
            Event::NodeProgress(NodeProgress {
                id,
                progress: Some(node_progress::Progress::Builds(builds)),
            }) => {
                let Some(span) = self.spans.get(&id) else {
                    return;
                };
                let activity = Arc::clone(&self.reporters.activity);
                span.in_scope(|| {
                    activity.progress(&BuildProgress {
                        builds_done: builds.builds_done,
                        builds_expected: builds.builds_expected,
                        builds_running: builds.builds_running,
                        downloads_done: builds.downloads_done,
                        downloads_expected: builds.downloads_expected,
                        downloads_running: builds.downloads_running,
                        bytes_done: builds.bytes_done,
                        bytes_expected: builds.bytes_expected,
                    })
                });
            }
            Event::Diagnostic(diagnostic) => tracing::warn!("{}", diagnostic.message),
            _ => {}
        }
    }
}

impl Render for Human {
    fn envelope(&mut self, envelope: Envelope) {
        self.replay(envelope);
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
    use mix_events::{Ending, Outbox, ROOT, Start, Tree};

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
        let mut bridge = Human::new(Reporters {
            downloads: seen.clone(),
            steps: seen.clone(),
            activity: Arc::new(mix_core::NoopActivity),
        });
        let mut tree = Tree::new(
            outbox.clone(),
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
        let action = tree
            .start(
                step,
                Start::new(
                    "action-1",
                    node_started::Kind::Action(mix_events::v1::Action {
                        operation: mix_events::v1::Operation::InstallRuntime as i32,
                        subject: "https://mirror/nix.tar.xz".into(),
                    }),
                ),
            )
            .unwrap();
        for done in [4, 10] {
            tree.progress(
                action,
                node_progress::Progress::Bytes(Bytes {
                    done,
                    total: Some(10),
                }),
            )
            .unwrap();
            outbox
                .drain()
                .into_iter()
                .for_each(|envelope| bridge.envelope(envelope));
        }
        tree.finish(action, Ending::succeeded()).unwrap();
        tree.finish(step, Ending::succeeded()).unwrap();
        outbox
            .drain()
            .into_iter()
            .for_each(|envelope| bridge.envelope(envelope));

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

    #[derive(Default)]
    struct Printed(Mutex<Vec<String>>);

    impl mix_core::ActivityReporter for Printed {
        fn line(&self, line: &str) {
            self.0.lock().unwrap().push(format!("line {line}"));
        }

        fn progress(&self, progress: &BuildProgress) {
            self.0.lock().unwrap().push(format!(
                "builds {}/{}",
                progress.builds_done, progress.builds_expected
            ));
        }

        fn clear(&self) {
            self.0.lock().unwrap().push("clear".into());
        }
    }

    fn action_under_a_step(tree: &mut Tree, key: &'static str) -> NodeId {
        let step = tree
            .start(
                ROOT,
                Start::new(key, node_started::Kind::Step(Step { title: key.into() })),
            )
            .unwrap();
        tree.start(
            step,
            Start::new(
                "action-1",
                node_started::Kind::Action(mix_events::v1::Action {
                    operation: mix_events::v1::Operation::ActivateProfile as i32,
                    subject: "ciuser".into(),
                }),
            ),
        )
        .unwrap()
    }

    #[test]
    fn nix_output_and_counters_reach_the_display_and_are_cleared_after_the_action() {
        let printed = Arc::new(Printed::default());
        let outbox = Arc::new(Outbox::new("request", || {}));
        let mut bridge = Human::new(Reporters {
            downloads: Arc::new(Seen::default()),
            steps: Arc::new(Seen::default()),
            activity: printed.clone(),
        });
        let mut tree = Tree::new(
            outbox.clone(),
            Arc::new(|| None),
            Start::command("install", Command::default()),
        );
        let quiet = action_under_a_step(&mut tree, "write-config");
        tree.finish(quiet, Ending::succeeded()).unwrap();
        let action = action_under_a_step(&mut tree, "activate");
        tree.progress(
            action,
            mix_events::output(b"building hello", mix_events::v1::Stream::Stderr),
        )
        .unwrap();
        tree.progress(
            action,
            node_progress::Progress::Builds(mix_events::v1::Builds {
                builds_done: 1,
                builds_expected: 3,
                ..Default::default()
            }),
        )
        .unwrap();
        tree.finish(action, Ending::succeeded()).unwrap();
        outbox
            .drain()
            .into_iter()
            .for_each(|envelope| bridge.envelope(envelope));

        assert_eq!(
            *printed.0.lock().unwrap(),
            ["line building hello", "builds 1/3", "clear"]
        );
    }
}
