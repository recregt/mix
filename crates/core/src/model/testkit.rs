use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_events::v1::command::Request;
use mix_events::v1::{Cancellation, Command, Envelope};
use mix_events::{Ending, Outbox, ROOT, Start, Tree};
use serde_json::{Value, json};

use crate::declared::state::StateManifest;
use crate::effect::{Action, Failure, Owner, Query};
use crate::model::{Content, Entry, Unit, World};
use crate::report::diagnose::diagnostic;
use crate::run::journal::{Record, Recovery};
use crate::run::{Input, Next, Report, Runner, Verdict, make_guard};

pub const REQUEST: &str = "request";

pub type Refusal = fn(&[Query]) -> Option<Failure>;

#[derive(Clone)]
pub struct Script {
    pub fail_at: Option<usize>,
    pub fail_when: Option<fn(&Action) -> bool>,
    pub stop_after: Option<usize>,
    pub fail_undo_at: Option<usize>,
    pub in_doubt_at: Option<usize>,
    pub unobservable: Option<Refusal>,
    pub fail_commit: bool,
    pub crash: Option<Crash>,
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

#[derive(Debug, Clone, Copy)]
pub struct Crash {
    pub at: usize,
    pub after: bool,
}

pub struct Run {
    pub ended: Option<Report>,
    pub stream: Vec<Envelope>,
    pub performed: Vec<Action>,
    pub shielded: Vec<bool>,
    pub changes: usize,
    pub undos: usize,
    pub journal: Vec<Record>,
    pub crashed: bool,
}

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

pub fn recover(world: &mut World, journal: &[Record]) {
    match crate::run::journal::recover(journal) {
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

pub fn required_with(extra: &[&str]) -> StateManifest {
    let mut manifest = StateManifest::seed();
    manifest
        .packages
        .extend(extra.iter().map(|p| p.to_string()));
    manifest.sorted()
}

pub fn listed_with(extra: &[&str]) -> Vec<String> {
    required_with(extra).packages
}

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
    pub fn report(&self) -> &Report {
        self.ended
            .as_ref()
            .expect("the run crashed before it ended")
    }

    #[cfg(test)]
    pub fn document(&self) -> Value {
        let document =
            mix_render::document::of(&self.stream).expect("a driven run always ends its root");
        serde_json::to_value(document).expect("a document always serialises")
    }

    pub fn performed(&self) -> Vec<String> {
        self.performed
            .iter()
            .map(|action| {
                let (operation, subject) = action.describe();
                format!("{operation:?} {subject}")
            })
            .collect()
    }

    #[cfg(test)]
    pub fn case(&self, before: &World, after: &World) -> Value {
        json!({
            "document": self.document(),
            "performed": self.performed(),
            "machine": difference(before, after),
        })
    }
}

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
    add("repositories", repositories(before, after));
    add(
        "files",
        compare_with(
            outside_repositories(before),
            outside_repositories(after),
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

fn inside_a_repository(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str() == ".git")
}

fn outside_repositories(world: &World) -> impl Iterator<Item = (String, &Entry)> {
    world
        .files
        .iter()
        .filter(|(path, _)| !inside_a_repository(path))
        .map(|(path, entry)| (path.display().to_string(), entry))
}

fn repositories(before: &World, after: &World) -> BTreeMap<String, Value> {
    let describe = |world: &World| -> BTreeMap<String, Value> {
        world
            .repositories()
            .into_iter()
            .map(|repository| {
                let (verifies, snapshot) = world.repository_at(&repository);
                let state = repository.parent().unwrap_or(&repository).to_path_buf();
                let recorded: BTreeMap<String, Value> = snapshot
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(name, bytes)| {
                        let current = world.contents(state.join(&name)) == Some(bytes.as_ref());
                        (
                            name,
                            Value::String(if current { "as on disk" } else { "older" }.into()),
                        )
                    })
                    .collect();
                (
                    repository.display().to_string(),
                    json!({ "verifies": verifies, "records": recorded }),
                )
            })
            .collect()
    };
    let (before, after) = (describe(before), describe(after));
    compare(
        before.iter().map(|(path, value)| (path.clone(), value)),
        after.iter().map(|(path, value)| (path.clone(), value)),
        |left, right| left == right,
        Value::clone,
    )
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    Removed,
    Swapped,
    Emptied,
    Altered,
    Unreadable,
    Stranger,
    Locked,
}

impl Damage {
    pub const ALL: [Damage; 7] = [
        Damage::Removed,
        Damage::Swapped,
        Damage::Emptied,
        Damage::Altered,
        Damage::Unreadable,
        Damage::Stranger,
        Damage::Locked,
    ];
}

const STRANGER: Owner = (4242, 4242);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakage {
    pub path: PathBuf,
    pub damage: Damage,
}

impl Breakage {
    pub fn apply(&self, world: &mut World) -> bool {
        let Some(entry) = world.files.get(&self.path).cloned() else {
            return false;
        };
        let path = &self.path;
        match (self.damage, &entry.content) {
            (Damage::Removed, _) => {
                world.files.retain(|found, _| !found.starts_with(path));
            }
            (Damage::Swapped, Content::Directory) => {
                world.files.retain(|found, _| !found.starts_with(path));
                world.with_file(path, b"swapped", entry.mode & 0o666, entry.owner);
            }
            (Damage::Swapped, Content::File(_)) => {
                world.files.remove(path);
                world.with_dir(path, entry.mode | 0o111, entry.owner);
            }
            (Damage::Emptied, Content::File(bytes)) if !bytes.is_empty() => {
                world.files.get_mut(path).expect("found above").content =
                    Content::File(Arc::from(&b""[..]));
            }
            (Damage::Altered, Content::File(bytes)) => {
                let mut altered = bytes.to_vec();
                match altered.last_mut() {
                    Some(last) => *last ^= 0x20,
                    None => altered.push(b'x'),
                }
                world.files.get_mut(path).expect("found above").content =
                    Content::File(Arc::from(altered));
            }
            (Damage::Unreadable, _) if entry.mode & 0o777 != 0 => {
                world.files.get_mut(path).expect("found above").mode = entry.mode & !0o777;
            }
            (Damage::Stranger, _) if entry.owner != STRANGER => {
                world.files.get_mut(path).expect("found above").owner = STRANGER;
            }
            (Damage::Locked, Content::File(_)) => {
                let mut name = path.file_name().unwrap_or_default().to_os_string();
                name.push(".lock");
                let lock = path.with_file_name(name);
                if world.files.contains_key(&lock) {
                    return false;
                }
                world.with_file(lock, b"", 0o644, entry.owner);
            }
            _ => return false,
        }
        true
    }
}

pub fn owned(before: &World, after: &World) -> Vec<PathBuf> {
    after
        .files
        .keys()
        .filter(|path| !before.files.contains_key(*path))
        .cloned()
        .collect()
}

pub fn breakages(paths: &[PathBuf]) -> Vec<Breakage> {
    paths
        .iter()
        .flat_map(|path| {
            Damage::ALL.iter().map(|damage| Breakage {
                path: path.clone(),
                damage: *damage,
            })
        })
        .collect()
}
