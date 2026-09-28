use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::v1::{
    Command, Diagnostic, Envelope, NodeFinished, NodeProgress, NodeStarted, NotRun, NotRunReason,
    Status, envelope::Event, node_finished, node_progress, node_started,
};

pub trait Sink: Send + Sync {
    fn emit(&self, envelope: Envelope);
}

pub type Stopped = Arc<dyn Fn() -> bool + Send + Sync>;

struct Shared {
    sink: Arc<dyn Sink>,
    stopped: Stopped,
    request: String,
    next: AtomicU64,
    seq: Mutex<u64>,
}

impl Shared {
    fn allocate(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    fn emit(&self, event: Event) {
        let mut seq = self.seq.lock().unwrap_or_else(PoisonError::into_inner);
        *seq += 1;
        self.sink.emit(Envelope {
            seq: *seq,
            request: self.request.clone(),
            event: Some(event),
        });
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ending {
    status: Status,
    diagnostic: Option<Diagnostic>,
    result: Option<node_finished::Result>,
    exit_code: u32,
}

impl Ending {
    pub fn new(status: Status) -> Self {
        Self {
            status,
            diagnostic: None,
            result: None,
            exit_code: 0,
        }
    }

    pub fn succeeded() -> Self {
        Self::new(Status::Succeeded)
    }

    pub fn already_satisfied() -> Self {
        Self::new(Status::AlreadySatisfied)
    }

    pub fn failed(diagnostic: Diagnostic) -> Self {
        Self::new(Status::Failed).with_diagnostic(diagnostic)
    }

    pub fn cancelled() -> Self {
        Self::new(Status::Cancelled)
    }

    pub fn with_diagnostic(mut self, diagnostic: Diagnostic) -> Self {
        self.diagnostic = Some(diagnostic);
        self
    }

    pub fn with_result(mut self, result: node_finished::Result) -> Self {
        self.result = Some(result);
        self
    }

    pub fn with_exit_code(mut self, exit_code: u32) -> Self {
        self.exit_code = exit_code;
        self
    }
}

#[derive(Default)]
struct Children {
    planned: Vec<String>,
    used: HashSet<String>,
}

pub struct Node<'parent> {
    shared: Arc<Shared>,
    id: u64,
    children: Mutex<Children>,
    finished: bool,
    _parent: PhantomData<&'parent ()>,
}

pub const ROOT: u64 = 1;

impl Node<'static> {
    pub fn root(
        sink: Arc<dyn Sink>,
        stopped: Stopped,
        request: impl Into<String>,
        key: impl Into<String>,
        command: Command,
        planned: Vec<String>,
    ) -> Self {
        let shared = Arc::new(Shared {
            sink,
            stopped,
            request: request.into(),
            next: AtomicU64::new(ROOT),
            seq: Mutex::new(0),
        });
        Self::start(
            shared,
            0,
            key.into(),
            node_started::Kind::Command(command),
            planned,
        )
    }
}

impl<'parent> Node<'parent> {
    fn start(
        shared: Arc<Shared>,
        parent: u64,
        key: String,
        kind: node_started::Kind,
        planned: Vec<String>,
    ) -> Self {
        let id = shared.allocate();
        shared.emit(Event::NodeStarted(NodeStarted {
            id,
            parent,
            key,
            planned: planned.clone(),
            kind: Some(kind),
        }));
        Node {
            shared,
            id,
            children: Mutex::new(Children {
                planned,
                used: HashSet::new(),
            }),
            finished: false,
            _parent: PhantomData,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn child(&self, key: impl Into<String>, kind: node_started::Kind) -> Node<'_> {
        self.planned_child(key, kind, Vec::new())
    }

    pub fn planned_child(
        &self,
        key: impl Into<String>,
        kind: node_started::Kind,
        planned: Vec<String>,
    ) -> Node<'_> {
        let key = key.into();
        self.claim(&key);
        Node::start(self.shared.clone(), self.id, key, kind, planned)
    }

    pub fn not_run(&self, key: impl Into<String>, reason: NotRunReason) {
        let key = key.into();
        self.claim(&key);
        self.emit_not_run(key, reason);
    }

    pub fn progress(&self, progress: node_progress::Progress) {
        self.shared.emit(Event::NodeProgress(NodeProgress {
            id: self.id,
            progress: Some(progress),
        }));
    }

    pub fn warn(&self, mut diagnostic: Diagnostic) {
        diagnostic.node = self.id;
        self.shared.emit(Event::Diagnostic(diagnostic));
    }

    pub fn finish(mut self, ending: Ending) {
        self.close(ending);
    }

    fn claim(&self, key: &str) {
        let fresh = self
            .children
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .used
            .insert(key.to_string());
        debug_assert!(fresh, "node {} already has a child keyed {key}", self.id);
    }

    fn emit_not_run(&self, key: String, reason: NotRunReason) {
        self.shared.emit(Event::NotRun(NotRun {
            parent: self.id,
            key,
            reason: reason as i32,
        }));
    }

    fn close(&mut self, ending: Ending) {
        let unreached: Vec<String> = {
            let children = self.children.lock().unwrap_or_else(PoisonError::into_inner);
            children
                .planned
                .iter()
                .filter(|key| !children.used.contains(*key))
                .cloned()
                .collect()
        };
        for key in unreached {
            self.emit_not_run(key, NotRunReason::NotReached);
        }
        self.shared.emit(Event::NodeFinished(NodeFinished {
            id: self.id,
            status: ending.status as i32,
            diagnostic: ending.diagnostic,
            exit_code: ending.exit_code,
            result: ending.result,
        }));
        self.finished = true;
    }
}

impl Drop for Node<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let status = if (self.shared.stopped)() {
            Status::Cancelled
        } else {
            Status::Failed
        };
        self.close(Ending::new(status));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::v1::{Bytes, Plan, Step};

    #[derive(Default)]
    struct Recorded(Mutex<Vec<Envelope>>);

    impl Sink for Recorded {
        fn emit(&self, envelope: Envelope) {
            self.0.lock().unwrap().push(envelope);
        }
    }

    impl Recorded {
        fn envelopes(&self) -> Vec<Envelope> {
            std::mem::take(&mut self.0.lock().unwrap())
        }

        fn take(&self) -> Vec<Event> {
            self.envelopes()
                .into_iter()
                .map(|envelope| envelope.event.unwrap())
                .collect()
        }
    }

    fn never() -> Stopped {
        Arc::new(|| false)
    }

    fn root(sink: &Arc<Recorded>, stopped: Stopped, planned: &[&str]) -> Node<'static> {
        Node::root(
            sink.clone(),
            stopped,
            "request",
            "bootstrap",
            Command::default(),
            planned.iter().map(|key| key.to_string()).collect(),
        )
    }

    fn step() -> node_started::Kind {
        node_started::Kind::Step(Step::default())
    }

    fn finished(event: &Event) -> Option<(u64, Status)> {
        match event {
            Event::NodeFinished(node) => Some((node.id, node.status())),
            _ => None,
        }
    }

    fn not_run(event: &Event) -> Option<(u64, &str, NotRunReason)> {
        match event {
            Event::NotRun(node) => Some((node.parent, node.key.as_str(), node.reason())),
            _ => None,
        }
    }

    #[test]
    fn the_root_is_node_one_and_children_take_the_next_ids() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        let plan = root.child("plan", node_started::Kind::Plan(Plan::default()));
        let first = plan.child("first", step());
        let first_id = first.id();
        first.finish(Ending::succeeded());
        let second = plan.child("second", step());

        assert_eq!((root.id(), plan.id(), first_id, second.id()), (1, 2, 3, 4));
    }

    #[test]
    fn a_child_names_its_parent_and_its_key() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        root.child("fetch-runtime", step())
            .finish(Ending::succeeded());
        root.finish(Ending::succeeded());

        let events = sink.take();
        let Event::NodeStarted(child) = &events[1] else {
            panic!("expected the child to start, got {:?}", events[1]);
        };
        assert_eq!((child.parent, child.key.as_str()), (1, "fetch-runtime"));
    }

    #[test]
    fn a_planned_key_that_never_ran_is_reported_as_not_reached() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &["one", "two", "three"]);
        root.child("one", step()).finish(Ending::succeeded());
        root.not_run("two", NotRunReason::Skipped);
        root.finish(Ending::failed(Diagnostic::default()));

        let events = sink.take();
        let reported: Vec<_> = events.iter().filter_map(not_run).collect();
        assert_eq!(
            reported,
            [
                (1, "two", NotRunReason::Skipped),
                (1, "three", NotRunReason::NotReached)
            ]
        );
        assert_eq!(finished(events.last().unwrap()), Some((1, Status::Failed)));
    }

    #[test]
    fn a_node_dropped_while_the_request_runs_is_failed() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        drop(root.child("step", step()));

        assert_eq!(
            sink.take().iter().filter_map(finished).collect::<Vec<_>>(),
            [(2, Status::Failed)]
        );
    }

    #[test]
    fn a_node_dropped_after_a_stop_is_cancelled() {
        let sink = Arc::new(Recorded::default());
        let stop = Arc::new(AtomicBool::new(false));
        let probe = stop.clone();
        let root = root(&sink, Arc::new(move || probe.load(Ordering::Relaxed)), &[]);
        let child = root.child("step", step());

        stop.store(true, Ordering::Relaxed);
        drop(child);
        drop(root);

        assert_eq!(
            sink.take().iter().filter_map(finished).collect::<Vec<_>>(),
            [(2, Status::Cancelled), (1, Status::Cancelled)]
        );
    }

    #[test]
    fn a_finished_node_is_not_finished_again_when_dropped() {
        let sink = Arc::new(Recorded::default());
        root(&sink, never(), &[]).finish(Ending::succeeded());

        assert_eq!(
            sink.take().iter().filter_map(finished).collect::<Vec<_>>(),
            [(1, Status::Succeeded)]
        );
    }

    #[test]
    fn progress_and_warnings_point_at_their_node() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        let download = root.child("download", step());
        download.progress(node_progress::Progress::Bytes(Bytes { done: 1, total: 2 }));
        download.warn(Diagnostic::default());

        let events = sink.take();
        let Event::NodeProgress(progress) = &events[2] else {
            panic!("expected progress, got {:?}", events[2]);
        };
        let Event::Diagnostic(warning) = &events[3] else {
            panic!("expected a warning, got {:?}", events[3]);
        };
        assert_eq!((progress.id, warning.node), (2, 2));
    }

    #[test]
    fn the_ending_carries_the_result_and_the_exit_code() {
        let sink = Arc::new(Recorded::default());
        root(&sink, never(), &[]).finish(
            Ending::succeeded()
                .with_result(node_finished::Result::Bootstrap(Default::default()))
                .with_exit_code(3),
        );

        let events = sink.take();
        let Event::NodeFinished(node) = &events[1] else {
            panic!("expected the root to finish, got {:?}", events[1]);
        };
        assert_eq!(node.exit_code, 3);
        assert!(node.result.is_some());
    }

    #[test]
    fn every_envelope_is_numbered_from_one_and_carries_the_request() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        root.child("check", step()).finish(Ending::succeeded());
        root.finish(Ending::succeeded());

        let envelopes = sink.envelopes();
        let seqs: Vec<u64> = envelopes.iter().map(|envelope| envelope.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4]);
        assert!(
            envelopes
                .iter()
                .all(|envelope| envelope.request == "request")
        );
    }

    #[test]
    fn children_on_many_threads_are_delivered_in_the_order_they_are_numbered() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        std::thread::scope(|scope| {
            for thread in 0..8 {
                let root = &root;
                scope.spawn(move || {
                    let child = root.child(format!("worker-{thread}"), step());
                    for _ in 0..200 {
                        child.progress(node_progress::Progress::Bytes(Bytes::default()));
                    }
                    child.finish(Ending::succeeded());
                });
            }
        });
        root.finish(Ending::succeeded());

        let seqs: Vec<u64> = sink
            .envelopes()
            .iter()
            .map(|envelope| envelope.seq)
            .collect();
        assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    }
}
