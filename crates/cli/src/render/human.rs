use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mix_core::BuildProgress;
use mix_events::v1::command::Request;
use mix_events::v1::{
    Envelope, NodeProgress, Status, envelope::Event, node_progress, node_started,
};
use mix_events::{NodeId, ROOT};
use mix_shell::render::Render;
use mix_ui::{Display, StepLine};

use crate::controls::{self, Stopping};

pub struct Human {
    display: Arc<dyn Display>,
    stopping: Option<Stopping>,
    stop_noticed: bool,
    results: bool,
    verbosity: u8,
    lines: HashMap<NodeId, Arc<dyn StepLine>>,
    titles: HashMap<NodeId, String>,
    actions: HashSet<NodeId>,
    printed: HashSet<NodeId>,
}

impl Human {
    pub fn new(display: Arc<dyn Display>) -> Self {
        Self {
            display,
            stopping: None,
            stop_noticed: false,
            results: true,
            verbosity: 0,
            lines: HashMap::new(),
            titles: HashMap::new(),
            actions: HashSet::new(),
            printed: HashSet::new(),
        }
    }

    pub fn verbosity(mut self, verbosity: u8) -> Self {
        self.verbosity = verbosity;
        self
    }

    pub fn without_results(mut self) -> Self {
        self.results = false;
        self
    }

    fn detail(&self, level: u8, line: impl FnOnce() -> String) {
        if self.verbosity >= level {
            mix_ui::detail(&line());
        }
    }

    fn replay(&mut self, envelope: Envelope) {
        let Some(event) = envelope.event else {
            return;
        };
        match event {
            Event::NodeStarted(node) => match node.kind {
                Some(node_started::Kind::Command(command)) => {
                    self.stopping = command.request.as_ref().and_then(stopping_for);
                }
                Some(node_started::Kind::Step(step)) => {
                    self.detail(1, || format!("running: {}", step.title));
                    self.lines.insert(node.id, self.display.step(&step.title));
                    self.titles.insert(node.id, step.title);
                }
                Some(node_started::Kind::Rollback(rollback)) => {
                    let title = self
                        .titles
                        .get(&rollback.undoes)
                        .cloned()
                        .unwrap_or_default();
                    self.detail(1, || format!("rolling back: {title}"));
                    self.lines.insert(node.id, self.display.step(&title));
                }
                Some(node_started::Kind::Action(_)) => {
                    if let Some(line) = self.lines.get(&node.parent).cloned() {
                        self.actions.insert(node.id);
                        self.lines.insert(node.id, line);
                    }
                }
                _ => {}
            },
            Event::NodeFinished(node) if node.id == ROOT => {
                super::results::finished(&node, self.results, self.verbosity > 0);
            }
            Event::NodeFinished(node) if self.actions.remove(&node.id) => {
                let line = self.lines.remove(&node.id);
                if self.printed.remove(&node.id)
                    && let Some(line) = line
                {
                    line.clear();
                }
            }
            Event::NodeFinished(node) => {
                let failed = node.status() == Status::Failed;
                if failed {
                    let message = node
                        .diagnostic
                        .as_ref()
                        .map(|diagnostic| diagnostic.message.clone())
                        .unwrap_or_default();
                    match self.titles.get(&node.id) {
                        Some(title) => {
                            self.detail(2, || format!("step failed: {title} ({message})"));
                        }
                        None if self.lines.contains_key(&node.id) => {
                            mix_ui::detail(&format!("rollback failed: {message}"));
                        }
                        None => {}
                    }
                }
                if let Some(line) = self.lines.remove(&node.id) {
                    line.finish(failed);
                }
            }
            Event::NodeProgress(NodeProgress {
                id,
                progress: Some(progress),
            }) => self.progress(id, progress),
            Event::Diagnostic(diagnostic) => {
                mix_ui::warn(crate::explain::render::warning(&diagnostic).message());
            }
            _ => {}
        }
    }

    fn progress(&mut self, id: NodeId, progress: node_progress::Progress) {
        use node_progress::Progress;

        match progress {
            Progress::Stopping(_) => {
                if let Some(stopping) = &self.stopping
                    && !std::mem::replace(&mut self.stop_noticed, true)
                {
                    mix_ui::warn(stopping.first);
                }
            }
            Progress::Command(command) => {
                self.detail(2, || format!("running command: {}", command.line));
            }
            Progress::Fetch(fetch) => self.detail(1, || format!("fetching {}", fetch.url)),
            Progress::Build(build) => {
                if !mix_ui::progress_enabled() {
                    self.detail(2, || format!("building {}", build.derivation));
                }
            }
            Progress::Bytes(bytes) => {
                if let Some(line) = self.lines.get(&id) {
                    line.bytes(bytes.done, bytes.total);
                }
            }
            Progress::Line(line) => {
                if !mix_ui::progress_enabled() {
                    self.detail(2, || line.text.clone());
                }
                if let Some(drawn) = self.lines.get(&id) {
                    self.printed.insert(id);
                    drawn.line(&line.text);
                }
            }
            Progress::Builds(builds) => {
                if let Some(line) = self.lines.get(&id) {
                    line.builds(&BuildProgress {
                        builds_done: builds.builds_done,
                        builds_expected: builds.builds_expected,
                        builds_running: builds.builds_running,
                        downloads_done: builds.downloads_done,
                        downloads_expected: builds.downloads_expected,
                        downloads_running: builds.downloads_running,
                        bytes_done: builds.bytes_done,
                        bytes_expected: builds.bytes_expected,
                    });
                }
            }
        }
    }
}

fn stopping_for(request: &Request) -> Option<Stopping> {
    match request {
        Request::Bootstrap(_) => Some(controls::BOOTSTRAP),
        Request::Repair(_) => Some(controls::REPAIR),
        Request::Install(_) | Request::Remove(_) => Some(controls::CHANGE),
        Request::Doctor(_) => None,
    }
}

impl Render for Human {
    fn envelope(&mut self, envelope: Envelope) {
        self.replay(envelope);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mix_events::v1::{Bytes, Command, Step};
    use mix_events::{Ending, Outbox, Start, Tree};

    use super::*;

    #[derive(Default)]
    struct Seen(Mutex<Vec<String>>);

    impl Seen {
        fn push(&self, entry: String) {
            self.0.lock().unwrap().push(entry);
        }
    }

    struct Recorded(Arc<Seen>, String);

    struct Recording(Arc<Seen>);

    impl Display for Recording {
        fn step(&self, title: &str) -> Arc<dyn StepLine> {
            self.0.push(format!("opened {title}"));
            Arc::new(Recorded(Arc::clone(&self.0), title.to_string()))
        }
    }

    impl StepLine for Recorded {
        fn bytes(&self, done: u64, total: Option<u64>) {
            self.0.push(format!("bytes {done}/{total:?}"));
        }

        fn line(&self, text: &str) {
            self.0.push(format!("line {text}"));
        }

        fn builds(&self, progress: &BuildProgress) {
            self.0.push(format!(
                "builds {}/{}",
                progress.builds_done, progress.builds_expected
            ));
        }

        fn clear(&self) {
            self.0.push("clear".into());
        }

        fn finish(&self, failed: bool) {
            self.0.push(format!("closed {} failed={failed}", self.1));
        }
    }

    fn rendered(seen: &Arc<Seen>, outbox: &Outbox, human: &mut Human) -> Vec<String> {
        outbox
            .drain()
            .into_iter()
            .for_each(|envelope| human.envelope(envelope));
        seen.0.lock().unwrap().clone()
    }

    fn action_under_a_step(tree: &mut Tree, key: &'static str) -> (NodeId, NodeId) {
        let step = tree
            .start(
                ROOT,
                Start::new(key, node_started::Kind::Step(Step { title: key.into() })),
            )
            .unwrap();
        let action = tree
            .start(
                step,
                Start::new(
                    "action-1",
                    node_started::Kind::Action(mix_events::v1::Action {
                        operation: mix_events::v1::Operation::ActivateProfile as i32,
                        subject: "ciuser".into(),
                    }),
                ),
            )
            .unwrap();
        (step, action)
    }

    #[test]
    fn a_step_and_its_download_reach_the_display() {
        let seen = Arc::new(Seen::default());
        let outbox = Arc::new(Outbox::new("request", || {}));
        let mut human = Human::new(Arc::new(Recording(Arc::clone(&seen))));
        let mut tree = Tree::new(
            outbox.clone(),
            Arc::new(|| None),
            Start::command("bootstrap", Command::default()),
        );
        let (step, action) = action_under_a_step(&mut tree, "fetch-runtime");
        for done in [4, 10] {
            tree.progress(
                action,
                node_progress::Progress::Bytes(Bytes {
                    done,
                    total: Some(10),
                }),
            )
            .unwrap();
            rendered(&seen, &outbox, &mut human);
        }
        tree.finish(action, Ending::succeeded()).unwrap();
        tree.finish(step, Ending::succeeded()).unwrap();

        assert_eq!(
            rendered(&seen, &outbox, &mut human),
            [
                "opened fetch-runtime",
                "bytes 4/Some(10)",
                "bytes 10/Some(10)",
                "closed fetch-runtime failed=false"
            ]
        );
    }

    #[test]
    fn nix_output_and_counters_reach_the_display_and_are_cleared_after_the_action() {
        let seen = Arc::new(Seen::default());
        let outbox = Arc::new(Outbox::new("request", || {}));
        let mut human = Human::new(Arc::new(Recording(Arc::clone(&seen))));
        let mut tree = Tree::new(
            outbox.clone(),
            Arc::new(|| None),
            Start::command("install", Command::default()),
        );
        let (_, quiet) = action_under_a_step(&mut tree, "write-config");
        tree.finish(quiet, Ending::succeeded()).unwrap();
        let (_, action) = action_under_a_step(&mut tree, "activate");
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

        assert_eq!(
            rendered(&seen, &outbox, &mut human),
            [
                "opened write-config",
                "opened activate",
                "line building hello",
                "builds 1/3",
                "clear"
            ]
        );
    }
}
