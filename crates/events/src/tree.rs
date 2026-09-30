use std::borrow::Cow;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustc_hash::FxHashSet;

use crate::outbox::Outbox;
use crate::v1::{
    Builds, Bytes, Cancellation, Command, Diagnostic, NodeFinished, NodeProgress, NodeStarted,
    NotRun, NotRunReason, OutputLine, Status, Stream, envelope::Event, node_finished,
    node_progress::Progress, node_started,
};

pub type Stopped = Arc<dyn Fn() -> Option<Cancellation> + Send + Sync>;

pub type NodeId = u64;

pub const ROOT: NodeId = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Misuse {
    #[error("node {0} is not running")]
    NotOpen(NodeId),

    #[error("node {id} cannot finish while its child {child} is running")]
    ChildrenOpen { id: NodeId, child: NodeId },

    #[error("node {parent} already has a child keyed {key}")]
    DuplicateKey { parent: NodeId, key: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Start {
    key: Cow<'static, str>,
    kind: node_started::Kind,
    planned: Vec<Cow<'static, str>>,
    shielded: bool,
}

impl Start {
    pub fn new(key: impl Into<Cow<'static, str>>, kind: node_started::Kind) -> Self {
        Self {
            key: key.into(),
            kind,
            planned: Vec::new(),
            shielded: false,
        }
    }

    pub fn command(key: impl Into<Cow<'static, str>>, command: Command) -> Self {
        Self::new(key, node_started::Kind::Command(command))
    }

    pub fn planned(mut self, keys: impl IntoIterator<Item = impl Into<Cow<'static, str>>>) -> Self {
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

    pub fn for_root(self, problems_remain: bool) -> Self {
        let exit_code = match self.status {
            Status::Failed => exit::FAILED,
            Status::Cancelled => exit::INTERRUPTED,
            _ if problems_remain => exit::PROBLEMS_REMAIN,
            _ => exit::SUCCEEDED,
        };
        self.with_exit_code(exit_code)
    }
}

pub mod exit {
    pub const SUCCEEDED: u32 = 0;
    pub const FAILED: u32 = 1;
    pub const USAGE: u32 = 2;
    pub const PROBLEMS_REMAIN: u32 = 3;
    pub const INTERRUPTED: u32 = 130;
}

pub fn output(bytes: &[u8], stream: Stream) -> Progress {
    Progress::Line(OutputLine {
        text: String::from_utf8_lossy(bytes).into_owned(),
        stream: stream as i32,
    })
}

struct Open {
    parent: NodeId,
    planned: Vec<Cow<'static, str>>,
    used: FxHashSet<Cow<'static, str>>,
    children: usize,
    bytes: Option<Bytes>,
    builds: Option<Builds>,
}

impl Open {
    fn changed(&mut self, progress: &Progress) -> bool {
        match progress {
            Progress::Line(_) => true,
            Progress::Builds(builds) => self.builds.replace(*builds) != Some(*builds),
            Progress::Bytes(bytes) => self.bytes.replace(*bytes) != Some(*bytes),
        }
    }
}

pub struct Tree {
    outbox: Arc<Outbox>,
    stopped: Stopped,
    next: NodeId,
    open: Vec<Option<Open>>,
}

impl Tree {
    pub fn new(outbox: Arc<Outbox>, stopped: Stopped, start: Start) -> Self {
        debug_assert!(matches!(start.kind, node_started::Kind::Command(_)));
        let mut tree = Self {
            outbox,
            stopped,
            next: ROOT,
            open: Vec::new(),
        };
        tree.open_node(0, start);
        tree
    }

    pub fn is_open(&self, id: NodeId) -> bool {
        self.get(id).is_some()
    }

    fn get(&self, id: NodeId) -> Option<&Open> {
        self.open.get(usize::try_from(id).ok()?)?.as_ref()
    }

    fn get_mut(&mut self, id: NodeId) -> Option<&mut Open> {
        self.open.get_mut(usize::try_from(id).ok()?)?.as_mut()
    }

    pub fn start(&mut self, parent: NodeId, mut start: Start) -> Result<NodeId, Misuse> {
        start.key = self.claim(parent, start.key)?;
        Ok(self.open_node(parent, start))
    }

    pub fn not_run(
        &mut self,
        parent: NodeId,
        key: impl Into<Cow<'static, str>>,
        reason: NotRunReason,
    ) -> Result<(), Misuse> {
        let key = self.claim(parent, key.into())?;
        self.emit_not_run(parent, key.into_owned(), reason);
        Ok(())
    }

    pub fn progress(&mut self, id: NodeId, progress: Progress) -> Result<(), Misuse> {
        let open = self.get_mut(id).ok_or(Misuse::NotOpen(id))?;
        if open.changed(&progress) {
            self.outbox.push(Event::NodeProgress(NodeProgress {
                id,
                progress: Some(progress),
            }));
        }
        Ok(())
    }

    pub fn warn(&mut self, id: NodeId, mut diagnostic: Diagnostic) -> Result<(), Misuse> {
        if !self.is_open(id) {
            return Err(Misuse::NotOpen(id));
        }
        diagnostic.node = id;
        self.outbox.push(Event::Diagnostic(diagnostic));
        Ok(())
    }

    pub fn finish(&mut self, id: NodeId, ending: Ending) -> Result<(), Misuse> {
        let open = self.get(id).ok_or(Misuse::NotOpen(id))?;
        if open.children > 0 {
            let child = self
                .open
                .iter()
                .zip(0..)
                .find_map(|(open, child)| {
                    open.as_ref()
                        .filter(|open| open.parent == id)
                        .map(|_| child)
                })
                .expect("a counted child is open");
            return Err(Misuse::ChildrenOpen { id, child });
        }
        let open = self.open[usize::try_from(id).expect("an open id indexes the table")]
            .take()
            .expect("checked above");
        if let Some(parent) = self.get_mut(open.parent) {
            parent.children -= 1;
        }
        for key in open.planned.iter().filter(|key| !open.used.contains(*key)) {
            self.emit_not_run(id, key.to_string(), NotRunReason::NotReached);
        }
        self.outbox.push(Event::NodeFinished(NodeFinished {
            id,
            status: ending.status as i32,
            diagnostic: ending.diagnostic.map(Box::new),
            exit_code: ending.exit_code,
            cancellation: ending.cancellation as i32,
            result: ending.result,
        }));
        Ok(())
    }

    pub fn abandon(&mut self, id: NodeId) -> Result<(), Misuse> {
        let ending = self.abandoned();
        let mut open: Vec<NodeId> = self
            .open
            .iter()
            .zip(0..)
            .filter(|(open, node)| open.is_some() && self.descends_from(*node, id))
            .map(|(_, node)| node)
            .collect();
        if open.is_empty() {
            return Err(Misuse::NotOpen(id));
        }
        open.sort_unstable_by(|a, b| b.cmp(a));
        for node in open {
            self.finish(node, ending.clone())
                .expect("a child always has a higher id than its parent");
        }
        Ok(())
    }

    fn abandoned(&self) -> Ending {
        match (self.stopped)() {
            Some(cause) => Ending::cancelled(cause),
            None => Ending::new(Status::Failed),
        }
    }

    fn descends_from(&self, mut node: NodeId, ancestor: NodeId) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            match self.get(node) {
                Some(open) => node = open.parent,
                None => return false,
            }
        }
    }

    fn claim(
        &mut self,
        parent: NodeId,
        key: Cow<'static, str>,
    ) -> Result<Cow<'static, str>, Misuse> {
        let open = self.get_mut(parent).ok_or(Misuse::NotOpen(parent))?;
        if !open.used.insert(key.clone()) {
            return Err(Misuse::DuplicateKey {
                parent,
                key: key.into_owned(),
            });
        }
        Ok(key)
    }

    fn open_node(&mut self, parent: NodeId, start: Start) -> NodeId {
        let id = self.next;
        self.next += 1;
        if let Some(parent) = self.get_mut(parent) {
            parent.children += 1;
        }
        self.outbox.push(Event::NodeStarted(NodeStarted {
            id,
            parent,
            key: start.key.into_owned(),
            planned: start.planned.iter().map(ToString::to_string).collect(),
            shielded: start.shielded,
            kind: Some(start.kind),
        }));
        let index = usize::try_from(id).expect("node ids fit in memory");
        if self.open.len() <= index {
            self.open.resize_with(index + 1, || None);
        }
        self.open[index] = Some(Open {
            parent,
            planned: start.planned,
            used: FxHashSet::default(),
            children: 0,
            bytes: None,
            builds: None,
        });
        id
    }

    fn emit_not_run(&self, parent: NodeId, key: String, reason: NotRunReason) {
        self.outbox.push(Event::NotRun(NotRun {
            parent,
            key,
            reason: reason as i32,
        }));
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if self.is_open(ROOT) {
            let _ = self.abandon(ROOT);
        }
    }
}

pub struct Node<'parent> {
    tree: Arc<Mutex<Tree>>,
    id: NodeId,
    finished: bool,
    _parent: PhantomData<&'parent ()>,
}

impl Node<'static> {
    pub fn root(outbox: Arc<Outbox>, stopped: Stopped, start: Start) -> Self {
        Node {
            tree: Arc::new(Mutex::new(Tree::new(outbox, stopped, start))),
            id: ROOT,
            finished: false,
            _parent: PhantomData,
        }
    }
}

impl<'parent> Node<'parent> {
    pub fn id(&self) -> NodeId {
        self.id
    }

    pub fn start(&self, start: Start) -> Node<'_> {
        let id = self
            .tree()
            .start(self.id, start)
            .unwrap_or_else(|misuse| panic!("{misuse}"));
        Node {
            tree: Arc::clone(&self.tree),
            id,
            finished: false,
            _parent: PhantomData,
        }
    }

    pub fn child(&self, key: impl Into<Cow<'static, str>>, kind: node_started::Kind) -> Node<'_> {
        self.start(Start::new(key, kind))
    }

    pub fn not_run(&self, key: impl Into<Cow<'static, str>>, reason: NotRunReason) {
        self.tree()
            .not_run(self.id, key, reason)
            .unwrap_or_else(|misuse| panic!("{misuse}"));
    }

    pub fn progress(&self, progress: Progress) {
        self.tree()
            .progress(self.id, progress)
            .expect("a borrowed node is running");
    }

    pub fn warn(&self, diagnostic: Diagnostic) {
        self.tree()
            .warn(self.id, diagnostic)
            .expect("a borrowed node is running");
    }

    pub fn finish(mut self, ending: Ending) {
        self.finished = true;
        self.tree()
            .finish(self.id, ending)
            .expect("a node's children finish before it can be moved");
    }

    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.tree.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Node<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut tree = self.tree();
        if tree.is_open(self.id) {
            let _ = tree.abandon(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

    fn root(sink: &Arc<Recorded>, stopped: Stopped, planned: &[&'static str]) -> Node<'static> {
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

    fn tree(stopped: Stopped) -> (Arc<Outbox>, Tree) {
        let outbox = Arc::new(Outbox::new("request", || {}));
        let tree = Tree::new(
            outbox.clone(),
            stopped,
            Start::command("bootstrap", Command::default()),
        );
        (outbox, tree)
    }

    #[test]
    fn a_tree_refuses_misuse_and_emits_nothing_for_it() {
        let (outbox, mut tree) = tree(never());
        let first = tree.start(ROOT, Start::new("step", step())).unwrap();
        outbox.drain();

        assert_eq!(
            tree.start(ROOT, Start::new("step", step())),
            Err(Misuse::DuplicateKey {
                parent: ROOT,
                key: "step".to_string()
            })
        );
        assert_eq!(
            tree.finish(ROOT, Ending::succeeded()),
            Err(Misuse::ChildrenOpen {
                id: ROOT,
                child: first
            })
        );
        assert_eq!(
            tree.progress(99, output(b"x", Stream::Stdout)),
            Err(Misuse::NotOpen(99))
        );
        assert_eq!(
            tree.not_run(99, "later", NotRunReason::Skipped),
            Err(Misuse::NotOpen(99))
        );
        tree.finish(first, Ending::succeeded()).unwrap();
        outbox.drain();
        assert_eq!(
            tree.finish(first, Ending::succeeded()),
            Err(Misuse::NotOpen(first))
        );
        assert!(outbox.drain().is_empty());
    }

    #[test]
    fn abandoning_a_node_closes_its_subtree_children_first_with_the_cause() {
        let (outbox, mut tree) = tree(Arc::new(|| Some(Cancellation::Interrupted)));
        let plan = tree
            .start(
                ROOT,
                Start::new("plan", node_started::Kind::Plan(Plan::default())),
            )
            .unwrap();
        let fetch = tree.start(plan, Start::new("fetch", step())).unwrap();
        let download = tree.start(fetch, Start::new("download", step())).unwrap();

        tree.abandon(plan).unwrap();

        let closed: Vec<(u64, Status, Cancellation)> = outbox
            .drain()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Some(Event::NodeFinished(node)) => {
                    Some((node.id, node.status(), node.cancellation()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            closed,
            [
                (download, Status::Cancelled, Cancellation::Interrupted),
                (fetch, Status::Cancelled, Cancellation::Interrupted),
                (plan, Status::Cancelled, Cancellation::Interrupted)
            ]
        );
        assert!(tree.is_open(ROOT));
    }

    #[test]
    fn dropping_a_tree_closes_every_node_it_left_open() {
        let (outbox, mut tree) = tree(never());
        let first = tree.start(ROOT, Start::new("step", step())).unwrap();
        tree.start(first, Start::new("inner", step())).unwrap();

        drop(tree);

        let failed = outbox
            .drain()
            .into_iter()
            .filter(|envelope| {
                matches!(&envelope.event, Some(Event::NodeFinished(node)) if node.status() == Status::Failed)
            })
            .count();
        assert_eq!(failed, 3);
    }
}
