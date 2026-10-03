#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mix_events::Render;
use mix_events::v1::command::Request;
use mix_events::v1::{
    Envelope, NodeFinished, NodeProgress, Status as Ended, envelope::Event, node_progress,
    node_started,
};
use mix_events::{Detail, Fault, NodeId, ROOT};
use mix_ui::{Display, Out, Severity, Status, StepLine};

use super::{trace, verbs};

pub struct Human {
    display: Arc<dyn Display>,
    out: Arc<dyn Out>,
    recorded: Option<Duration>,
    level: Detail,
    started: Instant,
    stop_noticed: bool,
    request: Option<Request>,
    lines: HashMap<NodeId, Arc<dyn StepLine>>,
    subjects: HashMap<NodeId, String>,
    undone: HashMap<NodeId, String>,
    pending: HashMap<NodeId, Status>,
    actions: HashSet<NodeId>,
}

impl Human {
    pub fn new(display: Arc<dyn Display>) -> Self {
        Self {
            display,
            out: Arc::new(mix_ui::Spaced::new(mix_ui::Stderr)),
            recorded: None,
            level: Detail::Step,
            started: Instant::now(),
            stop_noticed: false,
            request: None,
            lines: HashMap::new(),
            subjects: HashMap::new(),
            undone: HashMap::new(),
            pending: HashMap::new(),
            actions: HashSet::new(),
        }
    }

    pub fn level(mut self, level: Detail) -> Self {
        self.level = level;
        self
    }

    pub fn to(mut self, out: Arc<dyn Out>) -> Self {
        self.out = Arc::new(mix_ui::Spaced::new(out));
        self
    }

    pub fn at(&mut self, offset: Duration) {
        self.recorded = Some(offset);
    }

    fn shows(&self, detail: Detail) -> bool {
        self.level >= detail
    }

    fn status(&self, detail: Detail, status: Status, subject: &str) {
        if self.shows(detail) {
            mix_ui::status_to(self.out.as_ref(), status, subject);
        }
    }

    fn announce(&mut self, node: NodeId, status: Status, subject: &str) {
        self.status(Detail::Step, status, subject);
        self.lines.insert(node, self.display.step(status, subject));
    }

    fn replay(&mut self, envelope: Envelope) {
        let Some(event) = envelope.event else {
            return;
        };
        match event {
            Event::NodeStarted(node) => match node.kind {
                Some(node_started::Kind::Command(command)) => {
                    self.request = command.request;
                    if self.shows(Detail::Trace) {
                        mix_ui::note_to(
                            self.out.as_ref(),
                            &mix_ui::note!("request {}", envelope.request),
                            None,
                        );
                    }
                }
                Some(node_started::Kind::Step(step)) => {
                    self.pending.insert(node.id, verbs::step(step.verb()));
                    self.undone
                        .insert(node.id, verbs::undone(step.verb(), &step.subject));
                    self.subjects.insert(node.id, step.subject);
                }
                Some(node_started::Kind::Rollback(rollback)) => {
                    let subject = self
                        .undone
                        .get(&rollback.undoes)
                        .cloned()
                        .unwrap_or_default();
                    self.announce(node.id, Status::RollingBack, &subject);
                }
                Some(node_started::Kind::Action(action)) => {
                    if let Some(status) = self.pending.remove(&node.parent) {
                        let subject = self.subjects.get(&node.parent).cloned().unwrap_or_default();
                        self.announce(node.parent, status, &subject);
                    }
                    self.status(
                        Detail::Action,
                        verbs::action(action.operation()),
                        &action.subject,
                    );
                    if let Some(line) = self.lines.get(&node.parent).cloned() {
                        self.actions.insert(node.id);
                        self.lines.insert(node.id, line);
                    }
                }
                Some(node_started::Kind::LockWait(wait)) => {
                    let subject = match (&wait.holder, &wait.command) {
                        (Some(holder), Some(command)) => {
                            format!("waiting for {holder}'s `mix {command}` to finish")
                        }
                        _ => "waiting for another `mix` command to finish".to_string(),
                    };
                    self.status(Detail::Step, Status::Blocking, &subject);
                }
                Some(
                    node_started::Kind::Plan(_)
                    | node_started::Kind::Process(_)
                    | node_started::Kind::Download(_)
                    | node_started::Kind::Inspection(_)
                    | node_started::Kind::NixActivity(_),
                )
                | None => {}
            },
            Event::NodeFinished(node) if node.id == ROOT => {
                let elapsed = self.recorded.unwrap_or_else(|| self.started.elapsed());
                super::results::finished(
                    self.out.as_ref(),
                    &node,
                    self.request.as_ref(),
                    self.level,
                    elapsed,
                );
                self.outcome(&node, elapsed);
            }
            Event::NodeFinished(node) if self.actions.remove(&node.id) => {
                self.lines.remove(&node.id);
            }
            Event::NodeFinished(node) => {
                self.pending.remove(&node.id);
                if let Some(line) = self.lines.remove(&node.id) {
                    line.finish();
                }
            }
            Event::NodeProgress(NodeProgress {
                id,
                progress: Some(progress),
            }) => self.progress(id, progress),
            Event::Diagnostic(diagnostic) => {
                if self.shows(Detail::Step) {
                    let words = mix_explain::warning(&diagnostic);
                    let code = (self.shows(Detail::Action)
                        && diagnostic.code() != mix_events::v1::Code::Unspecified)
                        .then(|| mix_events::code::kebab(diagnostic.code()));
                    mix_ui::report_to(
                        self.out.as_ref(),
                        Severity::Warning,
                        &words
                            .report()
                            .code(code.as_deref())
                            .causes(mix_explain::evidence(&mix_events::Fault::Failed(
                                diagnostic.clone(),
                            ))),
                    );
                }
            }
            Event::NodeProgress(NodeProgress { progress: None, .. }) | Event::NotRun(_) => {}
        }
    }

    fn outcome(&self, node: &NodeFinished, elapsed: std::time::Duration) {
        let fault = match node.status() {
            Ended::Failed => match &node.diagnostic {
                Some(diagnostic) => Fault::Failed(diagnostic.as_ref().clone()),
                None => Fault::Failed(mix_events::v1::Diagnostic::default()),
            },
            Ended::Cancelled => {
                self.status(
                    Detail::Step,
                    Status::Cancelled,
                    &format!(
                        "`{}` in {}",
                        self.request.as_ref().map_or("mix", crate::words::name),
                        super::results::took(elapsed)
                    ),
                );
                return;
            }
            Ended::Unspecified | Ended::Succeeded | Ended::AlreadySatisfied => return,
        };
        let words = match &self.request {
            Some(request) => crate::words::outcome(request, &fault),
            None => mix_explain::outcome("mix", &"finish", &fault),
        };
        let code = self
            .shows(Detail::Action)
            .then(|| fault.code())
            .flatten()
            .map(mix_events::code::kebab);
        mix_ui::report_to(
            self.out.as_ref(),
            Severity::Error,
            &words
                .report()
                .code(code.as_deref())
                .causes(mix_explain::evidence(&fault)),
        );
    }

    fn progress(&mut self, id: NodeId, progress: node_progress::Progress) {
        use node_progress::Progress;

        match progress {
            Progress::Stopping(_) => {
                if self.shows(Detail::Step)
                    && let Some(note) = self.request.as_ref().and_then(crate::words::stopping)
                    && !std::mem::replace(&mut self.stop_noticed, true)
                {
                    mix_ui::note_to(
                        self.out.as_ref(),
                        &note,
                        Some(&crate::words::second_ctrl_c()),
                    );
                }
            }
            Progress::Command(command) => {
                self.status(
                    Detail::Action,
                    Status::Running,
                    &format!("`{}`", command.line),
                );
            }
            Progress::CommandFinished(finished) => {
                let subject = match (finished.exit_code, finished.signal) {
                    (Some(0), _) => return,
                    (Some(code), _) => format!("with status {code}: `{}`", finished.line),
                    (None, Some(signal)) => format!("on signal {signal}: `{}`", finished.line),
                    (None, None) => format!("`{}`", finished.line),
                };
                self.status(Detail::Trace, Status::Exited, &subject);
            }
            Progress::Fetch(fetch) => self.status(Detail::Action, Status::Fetching, &fetch.url),
            Progress::Build(build) => {
                self.status(Detail::Action, Status::Building, &build.derivation);
                if let Some(line) = self.lines.get(&id) {
                    line.item(&build.name);
                }
            }
            Progress::Substitution(substitution) => {
                self.status(Detail::Action, Status::Fetching, &substitution.path);
                if let Some(line) = self.lines.get(&id) {
                    line.item(&substitution.name);
                }
            }
            Progress::Bytes(bytes) => {
                if let Some(line) = self.lines.get(&id) {
                    line.bytes(bytes.done, bytes.total);
                }
            }
            Progress::Builds(builds) => {
                if let Some(line) = self.lines.get(&id) {
                    line.builds(&builds);
                }
            }
            Progress::Line(line) => {
                if self.shows(Detail::Trace) {
                    mix_ui::output_to(self.out.as_ref(), &line.text);
                }
            }
            Progress::Observed(observed) => {
                if self.shows(Detail::Trace) {
                    for observation in &observed.observations {
                        mix_ui::status_to(
                            self.out.as_ref(),
                            Status::Observed,
                            &trace::observation(observation),
                        );
                    }
                }
            }
            Progress::Journaled(journaled) => {
                if self.shows(Detail::Trace) {
                    mix_ui::status_to(
                        self.out.as_ref(),
                        Status::Journaled,
                        &trace::record(&journaled),
                    );
                }
            }
        }
    }
}

impl Render for Human {
    fn envelope(&mut self, envelope: Envelope) {
        self.replay(envelope);
    }

    fn detail(&self) -> Detail {
        self.level
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mix_events::v1::{Bytes, Command, Step, Verb};
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
        fn step(&self, status: Status, subject: &str) -> Arc<dyn StepLine> {
            self.0.push(format!("opened {} {subject}", status.text()));
            Arc::new(Recorded(Arc::clone(&self.0), subject.to_string()))
        }
    }

    impl StepLine for Recorded {
        fn bytes(&self, done: u64, total: Option<u64>) {
            self.0.push(format!("bytes {done}/{total:?}"));
        }

        fn builds(&self, progress: &mix_events::v1::Builds) {
            self.0.push(format!(
                "builds {}/{}",
                progress.builds_done, progress.builds_expected
            ));
        }

        fn item(&self, name: &str) {
            self.0.push(format!("item {name}"));
        }

        fn finish(&self) {
            self.0.push(format!("closed {}", self.1));
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
                Start::new(
                    key,
                    node_started::Kind::Step(Step {
                        verb: Verb::Activating as i32,
                        subject: key.into(),
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
                        operation: mix_events::v1::Operation::ActivateProfile as i32,
                        subject: "ciuser".into(),
                    }),
                ),
            )
            .unwrap();
        (step, action)
    }

    fn setup() -> (Arc<Seen>, Arc<Outbox>, Human, Tree) {
        let seen = Arc::new(Seen::default());
        let outbox = Arc::new(Outbox::new("request", || {}));
        let human = Human::new(Arc::new(Recording(Arc::clone(&seen)))).level(Detail::Outcome);
        let tree = Tree::new(
            outbox.clone(),
            Arc::new(|| None),
            Start::command("install", Command::default()),
        );
        (seen, outbox, human, tree)
    }

    #[test]
    fn a_step_and_its_download_reach_the_live_line() {
        let (seen, outbox, mut human, mut tree) = setup();
        let (step, action) = action_under_a_step(&mut tree, "profile");
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
                "opened Activating profile",
                "bytes 4/Some(10)",
                "bytes 10/Some(10)",
                "closed profile"
            ]
        );
    }

    #[test]
    fn counters_reach_the_live_line_and_a_programs_own_words_do_not() {
        let (seen, outbox, mut human, mut tree) = setup();
        let (step, action) = action_under_a_step(&mut tree, "profile");
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
        tree.finish(step, Ending::succeeded()).unwrap();

        assert_eq!(
            rendered(&seen, &outbox, &mut human),
            ["opened Activating profile", "builds 1/3", "closed profile"]
        );
    }
}
