//! One way to run a command's steps against the world model, inject faults into it, and describe
//! what came out for a snapshot: the result document, the actions performed and how the machine
//! changed.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;

use mix_events::v1::command::Request;
use mix_events::v1::{Cancellation, Command, Envelope};
use mix_events::{Ending, Outbox, ROOT, Start, Tree};
use serde_json::{Value, json};

use crate::action::{Action, Failure, Query};
use crate::journal::{Record, Recovery};
use crate::plan::{Input, Next, Report, Runner, Verdict, describe, diagnostic, make_guard};
use crate::world::{Content, Entry, Unit, World};

/// The request id every driven command reports, so documents compare across runs.
pub const REQUEST: &str = "request";

/// Answers a set of queries with a failure instead of facts, when it returns one.
pub type Refusal = fn(&[Query]) -> Option<Failure>;

/// Where a driven command meets trouble. Counts start at zero and leave the commit out.
#[derive(Clone)]
pub struct Script {
    /// The forward change that fails.
    pub fail_at: Option<usize>,
    /// Fails every forward change it accepts.
    pub fail_when: Option<fn(&Action) -> bool>,
    /// The forward change after which the command is interrupted.
    pub stop_after: Option<usize>,
    /// The undo that fails.
    pub fail_undo_at: Option<usize>,
    /// The forward change that takes effect and then reports a cancellation, leaving the runner
    /// in doubt whether it happened.
    pub in_doubt_at: Option<usize>,
    /// Answers a set of queries with this failure instead of facts, when it returns one.
    pub unobservable: Option<Refusal>,
    /// Whether the commit fails, after every change took effect.
    pub fail_commit: bool,
    /// The action, counted with undos and the commit, at which the machine loses power.
    pub crash: Option<Crash>,
    /// What a failing change or undo reports.
    pub failure: Failure,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            fail_at: None,
            fail_when: None,
            stop_after: None,
            fail_undo_at: None,
            in_doubt_at: None,
            fail_commit: false,
            unobservable: None,
            crash: None,
            failure: Failure::Io {
                path: "/injected".into(),
                kind: std::io::ErrorKind::Other,
            },
        }
    }
}

impl Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Script")
            .field("fail_at", &self.fail_at)
            .field("fail_when", &self.fail_when.is_some())
            .field("stop_after", &self.stop_after)
            .field("fail_undo_at", &self.fail_undo_at)
            .field("in_doubt_at", &self.in_doubt_at)
            .field("fail_commit", &self.fail_commit)
            .field("unobservable", &self.unobservable.is_some())
            .field("crash", &self.crash)
            .finish()
    }
}

impl Script {
    pub fn failing_at(change: usize) -> Self {
        Self {
            fail_at: Some(change),
            ..Self::default()
        }
    }

    pub fn failing_when(fail: fn(&Action) -> bool, failure: Failure) -> Self {
        Self {
            fail_when: Some(fail),
            failure,
            ..Self::default()
        }
    }

    pub fn stopping_after(change: usize) -> Self {
        Self {
            stop_after: Some(change),
            ..Self::default()
        }
    }

    pub fn crashing(at: usize, after: bool) -> Self {
        Self {
            crash: Some(Crash { at, after }),
            ..Self::default()
        }
    }
}

/// A power cut at one action: before it touches the machine, or after it did but before the
/// journal records it done.
#[derive(Debug, Clone, Copy)]
pub struct Crash {
    pub at: usize,
    pub after: bool,
}

/// What one driven command did.
pub struct Run {
    /// How the runner ended, or `None` when the machine crashed first.
    pub ended: Option<Report>,
    pub stream: Vec<Envelope>,
    pub performed: Vec<Action>,
    /// Whether each performed action ran shielded from a stop.
    pub shielded: Vec<bool>,
    /// Forward changes performed or attempted, the commit left out.
    pub changes: usize,
    /// Undos performed or attempted.
    pub undos: usize,
    /// What the request's journal holds, as the real journal writes it.
    pub journal: Vec<Record>,
    /// Whether the run ended in a crash, so nothing after it ran.
    pub crashed: bool,
}

/// Runs `runner` to its end against `world`, as `command` with the request id [`REQUEST`].
#[expect(
    clippy::too_many_lines,
    reason = "one loop answers every kind of fault"
)]
pub fn drive(world: &mut World, mut runner: Runner, command: Start, script: &Script) -> Run {
    make_guard!(guard);
    let mut runner = runner.brand(guard);
    let outbox = Arc::new(Outbox::new(REQUEST, || {}));
    let mut tree = Tree::new(Arc::clone(&outbox), Arc::new(|| None), command);
    let mut input = None;
    let mut performed = Vec::new();
    let mut shielded = Vec::new();
    let mut changes = 0;
    let mut undos = 0;
    let mut journal = vec![Record::Began {
        request: REQUEST.into(),
    }];
    let report = loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                let refused = script.unobservable.and_then(|refuse| refuse(&queries));
                input = Some(Input::Facts(match refused {
                    Some(failure) => Err(failure),
                    None => Ok(queries.iter().map(|query| world.observe(query)).collect()),
                }));
            }
            Next::Perform(action) => {
                let seq = performed.len();
                performed.push(action.clone());
                shielded.push(runner.shielded());
                if action == Action::Commit {
                    journal.push(Record::Committing);
                }
                let undo = world
                    .clone()
                    .apply(&action)
                    .map(|done| done.undo)
                    .unwrap_or_default();
                journal.push(Record::Prepared {
                    seq: seq as u64,
                    undo,
                });
                if let Some(crash) = script.crash.filter(|crash| crash.at == seq) {
                    if crash.after {
                        let _ = world.apply(&action);
                    }
                    return Run {
                        ended: None,
                        stream: outbox.drain(),
                        performed,
                        shielded,
                        changes,
                        undos,
                        journal,
                        crashed: true,
                    };
                }
                let outcome = if runner.rolling_back() {
                    undos += 1;
                    if script.fail_undo_at == Some(undos - 1) {
                        Err(script.failure.clone())
                    } else {
                        world.apply(&action)
                    }
                } else if action == Action::Commit && script.fail_commit {
                    Err(script.failure.clone())
                } else if action == Action::Commit {
                    world.apply(&action)
                } else {
                    changes += 1;
                    let fails = script.fail_at == Some(changes - 1)
                        || script.fail_when.is_some_and(|fail| fail(&action));
                    if fails {
                        Err(script.failure.clone())
                    } else if script.in_doubt_at == Some(changes - 1) {
                        let done = world.apply(&action).expect("the action applies");
                        runner.in_doubt(done.undo);
                        Err(Failure::Cancelled)
                    } else {
                        let outcome = world.apply(&action);
                        if script.stop_after == Some(changes - 1) {
                            runner.stop(Cancellation::Interrupted);
                        }
                        outcome
                    }
                };
                journal.push(Record::Done { seq: seq as u64 });
                if action == Action::Commit {
                    journal.push(Record::Ended);
                }
                input = Some(Input::Done(outcome));
            }
            Next::Finished(closed) => break runner.report(closed).clone(),
        }
    };
    let ending = match &report.verdict {
        Verdict::Succeeded => Ending::succeeded(),
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
    };
    tree.finish(ROOT, ending)
        .expect("the root is open until the run ends");
    drop(tree);
    let stream = outbox.drain();
    if let Err(invalid) = mix_events::validate(&stream) {
        panic!("the run's events break the protocol: {invalid:?}");
    }
    Run {
        ended: Some(report),
        stream,
        performed,
        shielded,
        changes,
        undos,
        journal,
        crashed: false,
    }
}

/// Puts `world` where the next command's recovery of `journal` leaves it.
pub fn recover(world: &mut World, journal: &[Record]) {
    match crate::journal::recover(journal) {
        Recovery::Nothing => {}
        Recovery::RollBack { uncertain, certain } => {
            for action in uncertain {
                let _ = world.apply(&action);
            }
            for action in certain {
                world.apply(&action).expect("a certain undo applies");
            }
        }
        Recovery::FinishCommit { .. } => {
            world.apply(&Action::Commit).expect("the commit finishes");
        }
    }
}

/// The start of a command that asks for `request`, named as `mix` names it.
pub fn requested(request: Request) -> Start {
    Start::command(
        mix_events::key_of(Some(&request)),
        Command {
            request: Some(request),
            ..Command::default()
        },
    )
}

impl Run {
    /// How the runner ended; a crashed run has no report.
    pub fn report(&self) -> &Report {
        self.ended
            .as_ref()
            .expect("the run crashed before it ended")
    }

    /// The document `--json` prints for this run.
    #[cfg(test)]
    pub fn document(&self) -> Value {
        let document =
            mix_render::document::of(&self.stream).expect("a driven run always ends its root");
        serde_json::to_value(document).expect("a document always serialises")
    }

    /// Every action the run performed, in order, as an operation and its subject.
    pub fn performed(&self) -> Vec<String> {
        self.performed
            .iter()
            .map(|action| {
                let (operation, subject) = describe(action);
                format!("{operation:?} {subject}")
            })
            .collect()
    }

    /// The run for a snapshot: its document, every action it performed in order, and what it
    /// changed on the machine it started from.
    #[cfg(test)]
    pub fn case(&self, before: &World, after: &World) -> Value {
        json!({
            "document": self.document(),
            "performed": self.performed(),
            "machine": difference(before, after),
        })
    }
}

/// What differs between two machines, by kind and then by name; equal machines give an empty
/// object.
pub fn difference(before: &World, after: &World) -> Value {
    let mut changed = serde_json::Map::new();
    let mut add = |kind: &str, entries: BTreeMap<String, Value>| {
        if !entries.is_empty() {
            changed.insert(
                kind.to_string(),
                Value::Object(entries.into_iter().collect()),
            );
        }
    };
    add(
        "files",
        compare_with(
            before
                .files
                .iter()
                .map(|(path, entry)| (path.display().to_string(), entry)),
            after
                .files
                .iter()
                .map(|(path, entry)| (path.display().to_string(), entry)),
            |left, right| {
                left.content == right.content
                    && left.mode == right.mode
                    && left.owner == right.owner
            },
            &file,
            file_change,
        ),
    );
    add(
        "groups",
        compare(
            before
                .groups
                .iter()
                .map(|(name, group)| (name.clone(), group)),
            after
                .groups
                .iter()
                .map(|(name, group)| (name.clone(), group)),
            |left, right| left == right,
            |group| json!({ "gid": group.gid, "members": group.members }),
        ),
    );
    add(
        "users",
        compare(
            before.users.iter().map(|(name, user)| (name.clone(), user)),
            after.users.iter().map(|(name, user)| (name.clone(), user)),
            |left, right| left == right,
            |user| {
                json!({
                    "uid": user.uid,
                    "gid": user.gid,
                    "home": user.home,
                    "shell": user.shell,
                    "comment": user.comment,
                })
            },
        ),
    );
    add(
        "units",
        compare(
            meaningful(before),
            meaningful(after),
            |left, right| left == right,
            unit,
        ),
    );
    add(
        "profiles",
        compare(
            before
                .profiles
                .iter()
                .map(|(uid, profile)| (uid.to_string(), profile)),
            after
                .profiles
                .iter()
                .map(|(uid, profile)| (uid.to_string(), profile)),
            |left, right| left == right,
            |profile| {
                json!({
                    "generations": profile.generations,
                    "active": profile.active,
                    "dangling": profile.dangling,
                })
            },
        ),
    );
    add(
        "journals",
        compare(
            before
                .journals
                .iter()
                .map(|journal| (journal.request.clone(), journal)),
            after
                .journals
                .iter()
                .map(|journal| (journal.request.clone(), journal)),
            |left, right| left == right,
            |journal| json!({ "pending": journal.pending }),
        ),
    );
    Value::Object(changed)
}

fn meaningful(world: &World) -> impl Iterator<Item = (String, &Unit)> {
    world
        .units
        .iter()
        .filter(|(_, unit)| **unit != Unit::default())
        .map(|(name, unit)| (name.clone(), unit))
}

fn compare<'w, T: 'w>(
    before: impl Iterator<Item = (String, &'w T)>,
    after: impl Iterator<Item = (String, &'w T)>,
    same: impl Fn(&T, &T) -> bool,
    describe: impl Fn(&T) -> Value,
) -> BTreeMap<String, Value> {
    compare_with(
        before,
        after,
        same,
        &describe,
        |left, right| json!({ "before": describe(left), "after": describe(right) }),
    )
}

fn compare_with<'w, T: 'w>(
    before: impl Iterator<Item = (String, &'w T)>,
    after: impl Iterator<Item = (String, &'w T)>,
    same: impl Fn(&T, &T) -> bool,
    describe: &impl Fn(&T) -> Value,
    changed_from: impl Fn(&T, &T) -> Value,
) -> BTreeMap<String, Value> {
    let before: BTreeMap<String, &T> = before.collect();
    let after: BTreeMap<String, &T> = after.collect();
    let mut changed = BTreeMap::new();
    for (name, left) in &before {
        match after.get(name) {
            None => {
                changed.insert(name.clone(), json!({ "removed": describe(left) }));
            }
            Some(right) if !same(left, right) => {
                changed.insert(name.clone(), changed_from(left, right));
            }
            Some(_) => {}
        }
    }
    for (name, right) in &after {
        if !before.contains_key(name) {
            changed.insert(name.clone(), json!({ "added": describe(right) }));
        }
    }
    changed
}

fn text(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.lines().collect(),
        Err(_) => json!({ "bytes": bytes.len() }),
    }
}

fn mode(entry: &Entry) -> String {
    format!("{:04o}", entry.mode)
}

fn owner(entry: &Entry) -> String {
    format!("{}:{}", entry.owner.0, entry.owner.1)
}

fn file(entry: &Entry) -> Value {
    match &entry.content {
        Content::Directory => {
            json!({ "directory": true, "mode": mode(entry), "owner": owner(entry) })
        }
        Content::File(bytes) => {
            json!({ "contents": text(bytes), "mode": mode(entry), "owner": owner(entry) })
        }
    }
}

/// A changed file as what changed: a line diff of its contents, and its mode and owner as
/// before and after.
fn file_change(before: &Entry, after: &Entry) -> Value {
    let mut changed = serde_json::Map::new();
    match (&before.content, &after.content) {
        (Content::File(left), Content::File(right)) if left != right => {
            let contents = match (std::str::from_utf8(left), std::str::from_utf8(right)) {
                (Ok(left), Ok(right)) => similar::TextDiff::from_lines(left, right)
                    .iter_all_changes()
                    .map(|change| {
                        let sign = match change.tag() {
                            similar::ChangeTag::Delete => '-',
                            similar::ChangeTag::Insert => '+',
                            similar::ChangeTag::Equal => ' ',
                        };
                        Value::String(format!("{sign}{}", change.value().trim_end_matches('\n')))
                    })
                    .collect(),
                _ => json!({ "before": text(left), "after": text(right) }),
            };
            changed.insert("contents".into(), contents);
        }
        (left, right) if left != right => {
            changed.insert("before".into(), file(before));
            changed.insert("after".into(), file(after));
        }
        _ => {}
    }
    if before.mode != after.mode {
        changed.insert(
            "mode".into(),
            json!({ "before": mode(before), "after": mode(after) }),
        );
    }
    if before.owner != after.owner {
        changed.insert(
            "owner".into(),
            json!({ "before": owner(before), "after": owner(after) }),
        );
    }
    Value::Object(changed)
}

fn unit(unit: &Unit) -> Value {
    json!({
        "loaded": unit.loaded.as_deref().map(text),
        "enabled": unit.enabled,
        "running": unit.running.as_deref().map(text),
    })
}
