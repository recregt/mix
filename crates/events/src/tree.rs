use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::outbox::Outbox;
use crate::v1::{
    Builds, Bytes, Cancellation, Command, Diagnostic, NodeFinished, NodeProgress, NodeStarted,
    NotRun, NotRunReason, OutputLine, Status, Stream, envelope::Event, node_finished,
    node_progress::Progress, node_started,
};

pub type Stopped = Arc<dyn Fn() -> Option<Cancellation> + Send + Sync>;

pub const ROOT: u64 = 1;

struct Shared {
    outbox: Arc<Outbox>,
    stopped: Stopped,
    next: AtomicU64,
}

impl Shared {
    fn allocate(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    fn emit(&self, event: Event) {
        self.outbox.push(event);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Start {
    key: String,
    kind: node_started::Kind,
    planned: Vec<String>,
    shielded: bool,
}

impl Start {
    pub fn new(key: impl Into<String>, kind: node_started::Kind) -> Self {
        Self {
            key: key.into(),
            kind,
            planned: Vec::new(),
            shielded: false,
        }
    }

    pub fn command(key: impl Into<String>, command: Command) -> Self {
        Self::new(key, node_started::Kind::Command(command))
    }

    pub fn planned(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.planned = keys.into_iter().map(Into::into).collect();
        self
    }

    pub fn shielded(mut self) -> Self {
        self.shielded = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ending {
    status: Status,
    cancellation: Cancellation,
    diagnostic: Option<Diagnostic>,
    result: Option<node_finished::Result>,
    exit_code: u32,
}

impl Ending {
    fn new(status: Status) -> Self {
        Self {
            status,
            cancellation: Cancellation::Unspecified,
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

    pub fn cancelled(cause: Cancellation) -> Self {
        Self {
            cancellation: cause,
            ..Self::new(Status::Cancelled)
        }
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

pub fn output(bytes: &[u8], stream: Stream) -> Progress {
    Progress::Line(OutputLine {
        text: String::from_utf8_lossy(bytes).into_owned(),
        stream: stream as i32,
    })
}

#[derive(Default)]
struct State {
    planned: Vec<String>,
    used: HashSet<String>,
    bytes: Option<Bytes>,
    builds: Option<Builds>,
}

impl State {
    fn changed(&mut self, progress: &Progress) -> bool {
        match progress {
            Progress::Line(_) => true,
            Progress::Builds(builds) => self.builds.replace(*builds) != Some(*builds),
            Progress::Bytes(bytes) => self.bytes.replace(*bytes) != Some(*bytes),
        }
    }
}

pub struct Node<'parent> {
    shared: Arc<Shared>,
    id: u64,
    state: Mutex<State>,
    finished: bool,
    _parent: PhantomData<&'parent ()>,
}

impl Node<'static> {
    pub fn root(outbox: Arc<Outbox>, stopped: Stopped, start: Start) -> Self {
        debug_assert!(matches!(start.kind, node_started::Kind::Command(_)));
        let shared = Arc::new(Shared {
            outbox,
            stopped,
            next: AtomicU64::new(ROOT),
        });
        Self::open(shared, 0, start)
    }
}

impl<'parent> Node<'parent> {
    fn open(shared: Arc<Shared>, parent: u64, start: Start) -> Self {
        let id = shared.allocate();
        shared.emit(Event::NodeStarted(NodeStarted {
            id,
            parent,
            key: start.key,
            planned: start.planned.clone(),
            shielded: start.shielded,
            kind: Some(start.kind),
        }));
        Node {
            shared,
            id,
            state: Mutex::new(State {
                planned: start.planned,
                ..State::default()
            }),
            finished: false,
            _parent: PhantomData,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn start(&self, start: Start) -> Node<'_> {
        self.claim(&start.key);
        Node::open(self.shared.clone(), self.id, start)
    }

    pub fn child(&self, key: impl Into<String>, kind: node_started::Kind) -> Node<'_> {
        self.start(Start::new(key, kind))
    }

    pub fn not_run(&self, key: impl Into<String>, reason: NotRunReason) {
        let key = key.into();
        self.claim(&key);
        self.emit_not_run(key, reason);
    }

    pub fn progress(&self, progress: Progress) {
        if !self.state().changed(&progress) {
            return;
        }
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

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn claim(&self, key: &str) {
        let fresh = self.state().used.insert(key.to_string());
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
            let state = self.state();
            state
                .planned
                .iter()
                .filter(|key| !state.used.contains(*key))
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
            cancellation: ending.cancellation as i32,
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
        let ending = match (self.shared.stopped)() {
            Some(cause) => Ending::cancelled(cause),
            None => Ending::new(Status::Failed),
        };
        self.close(ending);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::v1::{Envelope, Plan, Step};

    struct Recorded(Arc<Outbox>);

    impl Default for Recorded {
        fn default() -> Self {
            Self(Arc::new(Outbox::new("request", || {})))
        }
    }

    impl Recorded {
        fn envelopes(&self) -> Vec<Envelope> {
            self.0.drain()
        }

        fn take(&self) -> Vec<Event> {
            self.envelopes()
                .into_iter()
                .map(|envelope| envelope.event.unwrap())
                .collect()
        }
    }

    fn never() -> Stopped {
        Arc::new(|| None)
    }

    fn root(sink: &Arc<Recorded>, stopped: Stopped, planned: &[&str]) -> Node<'static> {
        Node::root(
            sink.0.clone(),
            stopped,
            Start::command("bootstrap", Command::default()).planned(planned.iter().copied()),
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
        let root = root(
            &sink,
            Arc::new(move || {
                probe
                    .load(Ordering::Relaxed)
                    .then_some(Cancellation::ClientGone)
            }),
            &[],
        );
        let child = root.child("step", step());

        stop.store(true, Ordering::Relaxed);
        drop(child);
        drop(root);

        let causes: Vec<(u64, Status, Cancellation)> = sink
            .take()
            .iter()
            .filter_map(|event| match event {
                Event::NodeFinished(node) => Some((node.id, node.status(), node.cancellation())),
                _ => None,
            })
            .collect();
        assert_eq!(
            causes,
            [
                (2, Status::Cancelled, Cancellation::ClientGone),
                (1, Status::Cancelled, Cancellation::ClientGone)
            ]
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
        download.progress(Progress::Bytes(Bytes {
            done: 1,
            total: Some(2),
        }));
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
                        child.progress(output(b"line", Stream::Stdout));
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

    #[test]
    fn a_shielded_node_says_so_when_it_starts() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        root.start(Start::new("activate", step()).shielded())
            .finish(Ending::succeeded());

        let events = sink.take();
        let Event::NodeStarted(child) = &events[1] else {
            panic!("expected the child to start, got {:?}", events[1]);
        };
        assert!(child.shielded);
    }

    fn bytes(done: u64) -> Progress {
        Progress::Bytes(Bytes {
            done,
            total: Some(100),
        })
    }

    fn progress_of(envelope: &Envelope) -> Option<u64> {
        match &envelope.event {
            Some(Event::NodeProgress(NodeProgress {
                progress: Some(Progress::Bytes(bytes)),
                ..
            })) => Some(bytes.done),
            _ => None,
        }
    }

    #[test]
    fn a_consumer_that_keeps_up_sees_every_change() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        let mut seen = Vec::new();
        for done in 0..=100 {
            root.progress(bytes(done));
            seen.extend(sink.envelopes().iter().filter_map(progress_of));
        }

        assert_eq!(seen, (0..=100).collect::<Vec<_>>());
    }

    #[test]
    fn a_consumer_that_falls_behind_gets_only_the_latest_snapshot_in_order() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        let download = root.child("download", step());
        for done in 0..=60 {
            download.progress(bytes(done));
        }
        download.progress(output(b"verifying", Stream::Stdout));
        for done in 61..=100 {
            download.progress(bytes(done));
        }
        download.finish(Ending::succeeded());
        root.finish(Ending::succeeded());

        let envelopes = sink.envelopes();
        let seqs: Vec<u64> = envelopes.iter().map(|envelope| envelope.seq).collect();
        let kinds: Vec<&str> = envelopes
            .iter()
            .map(|envelope| match &envelope.event {
                Some(Event::NodeStarted(_)) => "started",
                Some(Event::NodeProgress(NodeProgress {
                    progress: Some(Progress::Line(_)),
                    ..
                })) => "line",
                Some(Event::NodeProgress(_)) => "bytes",
                Some(Event::NodeFinished(_)) => "finished",
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(seqs, (1..=6).collect::<Vec<_>>());
        assert_eq!(
            kinds,
            [
                "started", "started", "line", "bytes", "finished", "finished"
            ]
        );
        assert_eq!(envelopes.iter().find_map(progress_of), Some(100));
    }

    #[test]
    fn output_lines_are_never_superseded() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        for _ in 0..50 {
            root.progress(output(b"line", Stream::Stderr));
        }

        let lines = sink
            .take()
            .into_iter()
            .filter(|event| matches!(event, Event::NodeProgress(_)))
            .count();
        assert_eq!(lines, 50);
    }

    #[test]
    fn each_node_keeps_its_own_latest_snapshot() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        let first = root.child("first", step());
        let second = root.child("second", step());
        first.progress(bytes(10));
        second.progress(bytes(20));
        first.progress(bytes(30));

        let latest: Vec<(u64, u64)> = sink
            .envelopes()
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Some(Event::NodeProgress(progress)) => {
                    progress_of(envelope).map(|done| (progress.id, done))
                }
                _ => None,
            })
            .collect();
        assert_eq!(latest, [(3, 20), (2, 30)]);
    }

    #[test]
    fn every_push_wakes_the_consumer() {
        let woken = Arc::new(AtomicU64::new(0));
        let count = woken.clone();
        let outbox = Arc::new(Outbox::new("request", move || {
            count.fetch_add(1, Ordering::Relaxed);
        }));
        let root = Node::root(
            outbox,
            never(),
            Start::command("doctor", Command::default()),
        );
        root.progress(bytes(1));
        root.finish(Ending::succeeded());

        assert_eq!(woken.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn build_counters_are_reported_only_when_they_change() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        let one = Builds {
            builds_done: 1,
            ..Builds::default()
        };
        let mut sent = 0;
        for builds in [Builds::default(), Builds::default(), one, one] {
            root.progress(Progress::Builds(builds));
            sent += sink
                .take()
                .into_iter()
                .filter(|event| matches!(event, Event::NodeProgress(_)))
                .count();
        }

        assert_eq!(sent, 2);
    }

    #[test]
    fn every_output_line_is_sent_with_its_stream_even_when_it_is_not_utf8() {
        let sink = Arc::new(Recorded::default());
        let root = root(&sink, never(), &[]);
        root.progress(output(b"same", Stream::Stdout));
        root.progress(output(b"same", Stream::Stdout));
        root.progress(output(b"bad \xff byte", Stream::Stderr));

        let lines: Vec<(String, Stream)> = sink
            .take()
            .into_iter()
            .filter_map(|event| match event {
                Event::NodeProgress(NodeProgress {
                    progress: Some(Progress::Line(line)),
                    ..
                }) => Some((line.text.clone(), line.stream())),
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            [
                ("same".to_string(), Stream::Stdout),
                ("same".to_string(), Stream::Stdout),
                ("bad \u{fffd} byte".to_string(), Stream::Stderr)
            ]
        );
    }
}
