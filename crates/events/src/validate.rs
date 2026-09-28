use std::collections::{HashMap, HashSet};

use crate::tree::ROOT;
use crate::v1::{
    Envelope, NodeFinished, NodeStarted, NotRun, NotRunReason, Status, envelope::Event,
    node_started::Kind,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Violation {
    #[error("event {found} arrived where {expected} was due (I1)")]
    SeqGap { expected: u64, found: u64 },

    #[error("event {seq} belongs to another request")]
    RequestChanged { seq: u64 },

    #[error("event {seq} is not the start of the root command node (I2)")]
    FirstNotRoot { seq: u64 },

    #[error("event {seq} arrived after the root finished (I2)")]
    AfterRoot { seq: u64 },

    #[error("a node was started with id 0 (I3)")]
    ZeroId,

    #[error("node {id} was started twice (I3)")]
    DuplicateId { id: u64 },

    #[error("node {id} names parent {parent}, which never started (I3)")]
    UnknownParent { id: u64, parent: u64 },

    #[error("node {id} started under {parent}, which had already finished (I3)")]
    ParentFinished { id: u64, parent: u64 },

    #[error("node {id} finished without having started (I4)")]
    UnknownNode { id: u64 },

    #[error("node {id} finished twice (I4)")]
    FinishedTwice { id: u64 },

    #[error("node {id} finished while its child {child} was still running (I4)")]
    ChildrenOpen { id: u64, child: u64 },

    #[error("node {parent} has two children keyed {key} (I5)")]
    DuplicateKey { parent: u64, key: String },

    #[error("node {id} finished without running or reporting its planned child {key} (I6)")]
    PlannedMissing { id: u64, key: String },

    #[error("an event refers to node {id}, which is not running (I7)")]
    NotOpen { id: u64 },

    #[error("rollback {id} undoes {undoes}, which is not a finished step beside it (I9)")]
    RollbackTarget { id: u64, undoes: u64 },

    #[error("node {id} finished without a status")]
    UnspecifiedStatus { id: u64 },

    #[error("child {key} of node {parent} was not run for no stated reason")]
    UnspecifiedReason { parent: u64, key: String },

    #[error("node {id} is not the root but carries an exit code")]
    ExitCodeOnChild { id: u64 },

    #[error("the stream ended while {} nodes were still running", open.len())]
    Truncated { open: Vec<u64> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Running,
    Finished(Status),
    NotRun(NotRunReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: Option<u64>,
    pub path: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validated {
    pub entries: Vec<Entry>,
}

impl Validated {
    pub fn outcome(&self, path: &str) -> Option<Outcome> {
        self.entries
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.outcome)
    }
}

struct Record {
    parent: u64,
    entry: usize,
    step: bool,
    planned: Vec<String>,
    keys: HashSet<String>,
    open_children: HashSet<u64>,
    finished: bool,
}

#[derive(Default)]
pub struct Validator {
    next_seq: u64,
    request: Option<String>,
    nodes: HashMap<u64, Record>,
    validated: Validated,
    root_finished: bool,
}

pub fn validate<'a>(
    envelopes: impl IntoIterator<Item = &'a Envelope>,
) -> Result<Validated, Violation> {
    let mut validator = Validator::new();
    for envelope in envelopes {
        validator.push(envelope)?;
    }
    validator.finish()
}

impl Validator {
    pub fn new() -> Self {
        Self {
            next_seq: 1,
            ..Self::default()
        }
    }

    pub fn push(&mut self, envelope: &Envelope) -> Result<(), Violation> {
        let seq = envelope.seq;
        if seq != self.next_seq {
            return Err(Violation::SeqGap {
                expected: self.next_seq,
                found: seq,
            });
        }
        self.next_seq += 1;
        match &self.request {
            None => self.request = Some(envelope.request.clone()),
            Some(request) if *request != envelope.request => {
                return Err(Violation::RequestChanged { seq });
            }
            Some(_) => {}
        }
        if self.root_finished {
            return Err(Violation::AfterRoot { seq });
        }
        let Some(event) = &envelope.event else {
            return Ok(());
        };
        if self.nodes.is_empty() && !is_root_start(event) {
            return Err(Violation::FirstNotRoot { seq });
        }
        match event {
            Event::NodeStarted(node) => self.started(node),
            Event::NodeFinished(node) => self.finished(node),
            Event::NotRun(not_run) => self.not_run(not_run),
            Event::NodeProgress(progress) => self.open(progress.id),
            Event::Diagnostic(diagnostic) => self.open(diagnostic.node),
            Event::Log(log) if log.node == 0 => Ok(()),
            Event::Log(log) => self.open(log.node),
        }
    }

    pub fn finish(self) -> Result<Validated, Violation> {
        if !self.root_finished {
            let mut open: Vec<u64> = self
                .nodes
                .iter()
                .filter(|(_, record)| !record.finished)
                .map(|(id, _)| *id)
                .collect();
            open.sort_unstable();
            return Err(Violation::Truncated { open });
        }
        Ok(self.validated)
    }

    fn started(&mut self, node: &NodeStarted) -> Result<(), Violation> {
        let id = node.id;
        if id == 0 {
            return Err(Violation::ZeroId);
        }
        if self.nodes.contains_key(&id) {
            return Err(Violation::DuplicateId { id });
        }
        let path = if id == ROOT && node.parent == 0 {
            node.key.clone()
        } else {
            let parent = self
                .nodes
                .get_mut(&node.parent)
                .ok_or(Violation::UnknownParent {
                    id,
                    parent: node.parent,
                })?;
            if parent.finished {
                return Err(Violation::ParentFinished {
                    id,
                    parent: node.parent,
                });
            }
            if !parent.keys.insert(node.key.clone()) {
                return Err(Violation::DuplicateKey {
                    parent: node.parent,
                    key: node.key.clone(),
                });
            }
            parent.open_children.insert(id);
            let parent_path = &self.validated.entries[parent.entry].path;
            format!("{parent_path}/{}", node.key)
        };
        if let Some(Kind::Rollback(rollback)) = &node.kind {
            let undoes = rollback.undoes;
            let valid = self.nodes.get(&undoes).is_some_and(|target| {
                target.step && target.finished && target.parent == node.parent
            });
            if !valid {
                return Err(Violation::RollbackTarget { id, undoes });
            }
        }
        self.validated.entries.push(Entry {
            id: Some(id),
            path,
            outcome: Outcome::Running,
        });
        self.nodes.insert(
            id,
            Record {
                parent: node.parent,
                entry: self.validated.entries.len() - 1,
                step: matches!(node.kind, Some(Kind::Step(_))),
                planned: node.planned.clone(),
                keys: HashSet::new(),
                open_children: HashSet::new(),
                finished: false,
            },
        );
        Ok(())
    }

    fn finished(&mut self, node: &NodeFinished) -> Result<(), Violation> {
        let id = node.id;
        let record = self.nodes.get(&id).ok_or(Violation::UnknownNode { id })?;
        if record.finished {
            return Err(Violation::FinishedTwice { id });
        }
        if let Some(child) = record.open_children.iter().min() {
            return Err(Violation::ChildrenOpen { id, child: *child });
        }
        if let Some(key) = record
            .planned
            .iter()
            .find(|key| !record.keys.contains(*key))
        {
            return Err(Violation::PlannedMissing {
                id,
                key: key.clone(),
            });
        }
        let status = node.status();
        if status == Status::Unspecified {
            return Err(Violation::UnspecifiedStatus { id });
        }
        if id != ROOT && node.exit_code != 0 {
            return Err(Violation::ExitCodeOnChild { id });
        }
        let parent = record.parent;
        let entry = record.entry;
        if let Some(record) = self.nodes.get_mut(&id) {
            record.finished = true;
        }
        if let Some(parent) = self.nodes.get_mut(&parent) {
            parent.open_children.remove(&id);
        }
        self.validated.entries[entry].outcome = Outcome::Finished(status);
        if id == ROOT {
            self.root_finished = true;
        }
        Ok(())
    }

    fn not_run(&mut self, not_run: &NotRun) -> Result<(), Violation> {
        let parent_id = not_run.parent;
        let parent = match self.nodes.get_mut(&parent_id) {
            Some(parent) if !parent.finished => parent,
            _ => return Err(Violation::NotOpen { id: parent_id }),
        };
        if !parent.keys.insert(not_run.key.clone()) {
            return Err(Violation::DuplicateKey {
                parent: parent_id,
                key: not_run.key.clone(),
            });
        }
        let reason = not_run.reason();
        if reason == NotRunReason::Unspecified {
            return Err(Violation::UnspecifiedReason {
                parent: parent_id,
                key: not_run.key.clone(),
            });
        }
        let path = format!(
            "{}/{}",
            self.validated.entries[parent.entry].path, not_run.key
        );
        self.validated.entries.push(Entry {
            id: None,
            path,
            outcome: Outcome::NotRun(reason),
        });
        Ok(())
    }

    fn open(&self, id: u64) -> Result<(), Violation> {
        match self.nodes.get(&id) {
            Some(record) if !record.finished => Ok(()),
            _ => Err(Violation::NotOpen { id }),
        }
    }
}

fn is_root_start(event: &Event) -> bool {
    matches!(
        event,
        Event::NodeStarted(NodeStarted {
            id: ROOT,
            parent: 0,
            kind: Some(Kind::Command(_)),
            ..
        })
    )
}
