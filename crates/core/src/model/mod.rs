#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod repository;

pub use repository::Snapshot;

use crate::declared::identity::InvokingUser;
use crate::declared::paths::{
    DEFAULT_PROFILE_NIX_ENV, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SOCKET_SRC, is_leftover,
};
use crate::effect::{
    Accounted, Accounts, Action, Expect, Fact, Failure, FileId, Ground, GroupFacts, Kind, Node,
    Outcome, Owner, PathFacts, Performed, ProfileFacts, Query, Spot, UnitFacts, UnitFailure,
    UnitOperation, UserFacts, Verdict, account_precondition, precondition,
};

pub use crate::declared::paths::SYSTEMD_UNIT_DIR as UNIT_DIR;

const ROOT: Owner = (0, 0);

/// File whose absence makes the model's repository fail to verify, as it makes a real one.
pub use crate::declared::paths::REPOSITORY_HEAD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Directory,
    File(Arc<[u8]>),
}

impl Entry {
    pub fn content_kind(&self) -> &'static str {
        match self.content {
            Content::Directory => "directory",
            Content::File(_) => "file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub content: Content,
    pub mode: u32,
    pub owner: Owner,
    pub id: FileId,
    pub changed: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Unit {
    pub loaded: Option<Arc<[u8]>>,
    pub enabled: bool,
    pub running: Option<Arc<[u8]>>,
    pub since: Option<u64>,
    pub stamp: Option<u64>,
}

impl Unit {
    fn held(&self) -> bool {
        self.running.is_some() || self.enabled
    }

    fn forget_unless_held(&mut self) {
        if !self.held() {
            self.loaded = None;
        }
    }

    fn seen(&self, file: Option<Arc<[u8]>>) -> Option<Arc<[u8]>> {
        if self.held() {
            self.loaded.clone()
        } else {
            file
        }
    }
}

impl PartialEq for Unit {
    fn eq(&self, other: &Self) -> bool {
        self.loaded == other.loaded
            && self.enabled == other.enabled
            && self.running == other.running
    }
}

impl Eq for Unit {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub generations: Vec<u64>,
    pub active: Option<u64>,
    pub built: BTreeMap<u64, Vec<Option<Arc<[u8]>>>>,
    pub dangling: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct World {
    pub files: BTreeMap<PathBuf, Entry>,
    pub groups: BTreeMap<String, GroupFacts>,
    pub users: BTreeMap<String, UserFacts>,
    pub units: BTreeMap<String, Unit>,
    pub profiles: BTreeMap<u32, Profile>,
    pub journals: Vec<crate::effect::Abandoned>,
    pub logs: BTreeMap<String, Vec<crate::run::journal::Record>>,
    pub held: std::collections::BTreeSet<String>,
    pub acting_for: String,
    pub runs_as: Option<Owner>,
    pub clobbered: BTreeMap<u32, Vec<PathBuf>>,
    pending: BTreeMap<String, Vec<PathBuf>>,
    next_ino: u64,
}

impl PartialEq for World {
    fn eq(&self, other: &Self) -> bool {
        self.files == other.files
            && self.groups == other.groups
            && self.users == other.users
            && self.meaningful_units().eq(other.meaningful_units())
            && self.profiles == other.profiles
            && self.journals == other.journals
            && self.logs == other.logs
            && self.clobbered == other.clobbered
            && self.pending() == other.pending()
    }
}

impl Eq for World {}

impl World {
    fn meaningful_units(&self) -> impl Iterator<Item = (&String, &Unit)> {
        self.units
            .iter()
            .filter(|(_, unit)| **unit != Unit::default())
    }
}

impl Ground for World {
    type Handle = ();

    fn spot(&self, path: &Path) -> (Spot, Option<()>) {
        (self.spot_of(path), None)
    }

    fn running(&self, path: &Path) -> Owner {
        self.running_for(path)
    }
}

impl World {
    fn spot_of(&self, path: &Path) -> Spot {
        if path.parent().is_none() {
            return Spot::Blocked(conflict(path, "a path below the root", "the root itself"));
        }
        if let Err(failure) = self.parent_is_dir(path) {
            return Spot::Blocked(failure);
        }
        match self.files.get(path) {
            None => Spot::Missing,
            Some(entry) => Spot::Present(Node {
                kind: match entry.content {
                    Content::Directory => Kind::Directory,
                    Content::File(_) => Kind::File,
                },
                id: entry.id,
                mode: entry.mode,
                owner: entry.owner,
            }),
        }
    }
}

impl Accounts for World {
    fn group(&self, name: &str) -> Option<GroupFacts> {
        self.groups.get(name).cloned()
    }

    fn user(&self, name: &str) -> Option<UserFacts> {
        self.users.get(name).cloned()
    }

    fn group_with_gid(&self, gid: u32) -> Option<String> {
        self.groups
            .iter()
            .find(|(_, group)| group.gid == gid)
            .map(|(name, _)| name.clone())
    }

    fn user_with_uid(&self, uid: u32) -> Option<String> {
        self.users
            .iter()
            .find(|(_, user)| user.uid == uid)
            .map(|(name, _)| name.clone())
    }

    fn primary_of(&self, gid: u32) -> Option<String> {
        self.users
            .iter()
            .find(|(_, user)| user.gid == gid)
            .map(|(name, _)| name.clone())
    }

    fn groups_of(&self, user: &str) -> Vec<String> {
        self.groups
            .iter()
            .filter(|(_, group)| group.members.iter().any(|member| member == user))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

impl Default for World {
    fn default() -> Self {
        let mut world = Self {
            files: BTreeMap::new(),
            groups: BTreeMap::new(),
            users: BTreeMap::new(),
            units: BTreeMap::new(),
            profiles: BTreeMap::new(),
            journals: Vec::new(),
            logs: BTreeMap::new(),
            held: std::collections::BTreeSet::new(),
            acting_for: "model".to_string(),
            runs_as: None,
            clobbered: BTreeMap::new(),
            pending: BTreeMap::new(),
            next_ino: 1,
        };
        for dir in [
            "/",
            "/etc",
            "/etc/systemd",
            UNIT_DIR,
            "/home",
            "/var",
            "/var/lib",
            "/var/empty",
        ] {
            world.with_dir(dir, 0o755, ROOT);
        }
        world
    }
}

fn conflict(subject: &Path, expected: impl Into<String>, found: impl Into<String>) -> Failure {
    Failure::Conflict {
        subject: subject.display().to_string(),
        expected: expected.into(),
        found: found.into(),
    }
}

fn account_conflict(
    subject: &str,
    expected: impl Into<String>,
    found: impl Into<String>,
) -> Failure {
    Failure::Conflict {
        subject: subject.to_string(),
        expected: expected.into(),
        found: found.into(),
    }
}

fn not_found(path: &Path) -> Failure {
    Failure::Io {
        path: path.to_path_buf(),
        kind: std::io::ErrorKind::NotFound,
    }
}

fn describe(entry: Option<&Entry>) -> String {
    match entry {
        None => "nothing".to_string(),
        Some(entry) => format!("{:?} {}:{}", entry.id, entry.id.dev, entry.id.ino),
    }
}

fn unit_path(unit: &str) -> PathBuf {
    Path::new(UNIT_DIR).join(unit)
}

fn done(undo: Vec<Action>) -> Outcome {
    Ok(Performed { undo })
}

impl World {
    pub fn with_dir(&mut self, path: impl Into<PathBuf>, mode: u32, owner: Owner) -> &mut Self {
        let id = self.fresh();
        self.files.insert(
            path.into(),
            Entry {
                content: Content::Directory,
                mode,
                owner,
                id,
                changed: id.ino,
            },
        );
        self
    }

    pub fn with_file(
        &mut self,
        path: impl Into<PathBuf>,
        contents: &[u8],
        mode: u32,
        owner: Owner,
    ) -> &mut Self {
        let id = self.fresh();
        self.files.insert(
            path.into(),
            Entry {
                content: Content::File(contents.into()),
                mode,
                owner,
                id,
                changed: id.ino,
            },
        );
        self
    }

    pub fn contents(&self, path: impl AsRef<Path>) -> Option<&[u8]> {
        match self.files.get(path.as_ref()).map(|entry| &entry.content) {
            Some(Content::File(contents)) => Some(contents),
            _ => None,
        }
    }

    fn stamp(&self, path: impl AsRef<Path>) -> Option<u64> {
        self.files.get(path.as_ref()).map(|entry| entry.changed)
    }

    pub fn now(&self) -> u64 {
        self.next_ino
    }

    pub fn pending(&self) -> Vec<PathBuf> {
        self.pending.values().flatten().cloned().collect()
    }

    pub fn adopt(&mut self, paths: Vec<PathBuf>) {
        self.pending
            .entry(self.acting_for.clone())
            .or_default()
            .extend(paths);
    }

    pub fn forget(&mut self, request: &str) {
        self.pending.remove(request);
    }

    fn pend(&mut self, path: PathBuf) {
        self.pending
            .entry(self.acting_for.clone())
            .or_default()
            .push(path);
    }

    fn fresh(&mut self) -> FileId {
        let ino = self.next_ino;
        self.next_ino += 1;
        FileId {
            dev: 1,
            ino,
            born: Some((ino as i64, 0)),
        }
    }

    fn parent_is_dir(&self, path: &Path) -> Result<(), Failure> {
        let mut above: Vec<&Path> = path.ancestors().skip(1).collect();
        above.reverse();
        for dir in above {
            match self.files.get(dir).map(|entry| &entry.content) {
                Some(Content::Directory) => {}
                Some(Content::File(_)) => {
                    return Err(Failure::Io {
                        path: path.to_path_buf(),
                        kind: std::io::ErrorKind::NotADirectory,
                    });
                }
                None => return Err(not_found(path)),
            }
        }
        Ok(())
    }

    fn has_children(&self, path: &Path) -> bool {
        self.files
            .range(path.to_path_buf()..)
            .nth(1)
            .is_some_and(|(next, _)| next.starts_with(path))
    }

    fn subtree(&self, path: &Path) -> Vec<PathBuf> {
        self.files
            .range(path.to_path_buf()..)
            .take_while(|(next, _)| next.starts_with(path))
            .map(|(next, _)| next.clone())
            .collect()
    }

    fn move_tree(&mut self, from: &Path, to: &Path) {
        for old in self.subtree(from) {
            let entry = self.files.remove(&old).expect("listed above");
            let rest = old.strip_prefix(from).expect("inside the subtree");
            let new = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            self.files.insert(new, entry);
        }
    }

    fn sibling(&mut self, path: &Path, purpose: &str) -> PathBuf {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let n = self.fresh().ino;
        path.with_file_name(format!(".{name}.mix-{purpose}-{}-{n}", self.acting_for))
    }

    fn put(
        &mut self,
        path: &Path,
        contents: &Arc<[u8]>,
        mode: u32,
        owner: Owner,
        expect: Expect,
    ) -> Result<Vec<Action>, Failure> {
        precondition(
            &Action::PutFile {
                path: path.to_path_buf(),
                contents: Arc::clone(contents),
                mode,
                owner: Some(owner),
                expect,
            },
            self,
            &mut Vec::new(),
        )?;
        let id = self.fresh();
        let entry = Entry {
            content: Content::File(Arc::clone(contents)),
            mode,
            owner,
            id,
            changed: id.ino,
        };
        match expect {
            Expect::Absent => {
                self.files.insert(path.to_path_buf(), entry);
                Ok(vec![Action::RemoveCreated {
                    path: path.to_path_buf(),
                    expect: id,
                }])
            }
            Expect::Present(_) => {
                let backup = self.sibling(path, "backup");
                self.move_tree(path, &backup);
                self.pend(backup.clone());
                self.files.insert(path.to_path_buf(), entry);
                Ok(vec![Action::Restore {
                    path: path.to_path_buf(),
                    from: backup,
                    expect: Expect::Present(id),
                }])
            }
        }
    }

    pub fn apply(&mut self, action: &Action) -> Outcome {
        if self.already(action) {
            return done(Vec::new());
        }
        let verdict = match action {
            Action::PutFile { .. } => Verdict::Go,
            action => precondition(action, self, &mut Vec::new())?,
        };
        if verdict == Verdict::Done {
            return done(Vec::new());
        }
        let undo = match account_precondition(action, self)? {
            Accounted::Done => return done(Vec::new()),
            Accounted::Go(undo) => undo,
        };
        match action {
            Action::CreateDir { path, mode, owner } => {
                let id = self.fresh();
                self.files.insert(
                    path.clone(),
                    Entry {
                        content: Content::Directory,
                        mode: *mode,
                        owner: owner.unwrap_or(ROOT),
                        id,
                        changed: id.ino,
                    },
                );
                done(vec![Action::RemoveCreated {
                    path: path.clone(),
                    expect: id,
                }])
            }
            Action::CreateDirs { path, mode, owner } => {
                let Verdict::Below(top) = verdict else {
                    return Err(conflict(path, "a top to create", "none"));
                };
                let mut missing: Vec<PathBuf> = path
                    .ancestors()
                    .take_while(|dir| dir.starts_with(&top))
                    .map(Path::to_path_buf)
                    .collect();
                missing.reverse();
                let mut top_id = None;
                for dir in missing {
                    let id = self.fresh();
                    top_id.get_or_insert(id);
                    self.files.insert(
                        dir.clone(),
                        Entry {
                            content: Content::Directory,
                            mode: if dir == *path { *mode } else { 0o755 },
                            owner: owner.unwrap_or(ROOT),
                            id,
                            changed: id.ino,
                        },
                    );
                }
                done(vec![Action::RemoveCreatedTree {
                    path: top,
                    expect: top_id.expect("at least the top was created"),
                }])
            }
            Action::PutFile {
                path,
                contents,
                mode,
                owner,
                expect,
            } => done(self.put(path, contents, *mode, owner.unwrap_or(ROOT), *expect)?),
            Action::SetMode { path, mode, expect } => {
                let entry = self.files.get_mut(path).ok_or_else(|| not_found(path))?;
                entry.mode = *mode;
                done(vec![Action::SetMode {
                    path: path.clone(),
                    mode: *expect,
                    expect: *mode,
                }])
            }
            Action::SetOwner {
                path,
                owner,
                expect,
            } => {
                let entry = self.files.get_mut(path).ok_or_else(|| not_found(path))?;
                entry.owner = *owner;
                done(vec![Action::SetOwner {
                    path: path.clone(),
                    owner: *expect,
                    expect: *owner,
                }])
            }
            Action::SetAside { path, .. } => {
                let aside = self.sibling(path, "aside");
                self.move_tree(path, &aside);
                self.pend(aside.clone());
                done(vec![Action::Restore {
                    path: path.clone(),
                    from: aside,
                    expect: Expect::Absent,
                }])
            }
            Action::ReclaimTree {
                path, owner, mode, ..
            } => {
                let aside = self.sibling(path, "aside");
                let mut copy = Vec::new();
                for old in self.subtree(path) {
                    let entry = &self.files[&old];
                    let top = old == *path;
                    copy.push((
                        old.clone(),
                        Entry {
                            content: entry.content.clone(),
                            mode: if top { *mode } else { entry.mode & !0o6000 },
                            owner: *owner,
                            id: FileId {
                                dev: 0,
                                ino: 0,
                                born: None,
                            },
                            changed: entry.changed,
                        },
                    ));
                }
                self.move_tree(path, &aside);
                let mut top = None;
                for (at, mut entry) in copy {
                    entry.id = self.fresh();
                    entry.changed = entry.id.ino;
                    if at == *path {
                        top = Some(entry.id);
                    }
                    self.files.insert(at, entry);
                }
                self.pend(aside.clone());
                done(vec![
                    Action::RemoveCreatedTree {
                        path: path.clone(),
                        expect: top.expect("the top was copied"),
                    },
                    Action::Restore {
                        path: path.clone(),
                        from: aside,
                        expect: Expect::Absent,
                    },
                ])
            }
            Action::CopyTree {
                from,
                to,
                owner,
                mode,
            } => {
                let copy: Vec<(PathBuf, Entry)> = self
                    .subtree(from)
                    .into_iter()
                    .map(|old| {
                        let entry = self.files[&old].clone();
                        let rest = old.strip_prefix(from).expect("inside the subtree");
                        let new = if rest.as_os_str().is_empty() {
                            to.clone()
                        } else {
                            to.join(rest)
                        };
                        let top = old == *from;
                        (
                            new,
                            Entry {
                                mode: if top { *mode } else { entry.mode & !0o6000 },
                                owner: *owner,
                                ..entry
                            },
                        )
                    })
                    .collect();
                let mut top = None;
                for (at, mut entry) in copy {
                    entry.id = self.fresh();
                    entry.changed = entry.id.ino;
                    if at == *to {
                        top = Some(entry.id);
                    }
                    self.files.insert(at, entry);
                }
                done(vec![Action::RemoveCreatedTree {
                    path: to.clone(),
                    expect: top.expect("the top was copied"),
                }])
            }
            Action::RemoveCreated { path, .. } => {
                if self.has_children(path) {
                    return Err(conflict(
                        path,
                        "an empty directory",
                        "a directory with contents",
                    ));
                }
                self.files.remove(path);
                done(Vec::new())
            }
            Action::RemoveCreatedTree { path, .. } => {
                for entry in self.subtree(path) {
                    self.files.remove(&entry);
                }
                done(Vec::new())
            }
            Action::Restore { path, from, .. } => {
                for discarded in self.subtree(path) {
                    self.files.remove(&discarded);
                }
                self.move_tree(from, path);
                for pending in self.pending.values_mut() {
                    pending.retain(|pending| pending != from);
                }
                done(Vec::new())
            }
            Action::AddGroup { name, gid } => {
                self.groups.insert(
                    name.clone(),
                    GroupFacts {
                        gid: *gid,
                        members: Vec::new(),
                    },
                );
                done(undo)
            }
            Action::SetGroupGid { name, gid, expect } => {
                if let Some(group) = self.groups.get_mut(name) {
                    group.gid = *gid;
                }
                for user in self.users.values_mut() {
                    if user.gid == *expect {
                        user.gid = *gid;
                    }
                }
                done(undo)
            }
            Action::DeleteGroup { name, .. } => {
                self.groups.remove(name);
                done(undo)
            }
            Action::AddUser(spec) => {
                self.users.insert(
                    spec.name.clone(),
                    UserFacts {
                        uid: spec.uid,
                        gid: spec.gid,
                        home: spec.home.clone(),
                        shell: spec.shell.clone(),
                        comment: spec.comment.clone(),
                    },
                );
                for group in &spec.groups {
                    if let Some(group) = self.groups.get_mut(group) {
                        group.members.push(spec.name.clone());
                        group.members.sort();
                    }
                }
                done(undo)
            }
            Action::SetUserIds { name, ids, .. } => {
                if let Some(user) = self.users.get_mut(name) {
                    (user.uid, user.gid) = *ids;
                }
                done(undo)
            }
            Action::DeleteUser { name, .. } => {
                self.users.remove(name);
                for facts in self.groups.values_mut() {
                    facts.members.retain(|member| member != name);
                }
                done(undo)
            }
            Action::AddMember { group, user } => {
                if let Some(facts) = self.groups.get_mut(group) {
                    facts.members.push(user.clone());
                    facts.members.sort();
                }
                done(undo)
            }
            Action::RemoveMember { group, user } => {
                if let Some(facts) = self.groups.get_mut(group) {
                    facts.members.retain(|member| member != user);
                }
                done(undo)
            }
            Action::InstallUnit {
                unit,
                contents,
                expect,
            } => {
                let mut undo = self.put(&unit_path(unit), contents, 0o644, ROOT, *expect)?;
                undo.push(Action::DaemonReload);
                done(undo)
            }
            Action::DaemonReload => {
                self.reload();
                done(vec![Action::DaemonReload])
            }
            Action::EnableUnit { unit } => {
                let facts = self.loaded_unit(unit, UnitOperation::Enable)?;
                if facts.enabled {
                    return Err(account_conflict(unit, "disabled", "enabled"));
                }
                facts.enabled = true;
                self.reload();
                done(vec![Action::DisableUnit { unit: unit.clone() }])
            }
            Action::DisableUnit { unit } => {
                let facts = self.units.entry(unit.clone()).or_default();
                if !facts.enabled {
                    return Err(account_conflict(unit, "enabled", "disabled"));
                }
                facts.enabled = false;
                facts.forget_unless_held();
                self.reload();
                done(vec![Action::EnableUnit { unit: unit.clone() }])
            }
            Action::StartUnit { unit } => {
                let facts = self.loaded_unit(unit, UnitOperation::Start)?;
                if facts.running.is_some() {
                    return Err(account_conflict(unit, "inactive", "active"));
                }
                facts.running = facts.loaded.clone();
                let since = self.fresh().ino;
                self.units.get_mut(unit).expect("loaded above").since = Some(since);
                done(vec![Action::StopUnit { unit: unit.clone() }])
            }
            Action::StopUnit { unit } => {
                let facts = self.units.entry(unit.clone()).or_default();
                if facts.running.is_none() {
                    return Err(account_conflict(unit, "active", "inactive"));
                }
                facts.running = None;
                facts.since = None;
                facts.forget_unless_held();
                done(vec![Action::StartUnit { unit: unit.clone() }])
            }
            Action::RestartUnit { unit } => {
                let since = self.fresh().ino;
                let facts = self.units.entry(unit.clone()).or_default();
                if facts.running.is_none() {
                    return done(Vec::new());
                }
                facts.running = facts.loaded.clone();
                facts.since = Some(since);
                done(vec![Action::RestartUnit { unit: unit.clone() }])
            }
            Action::DrainService { unit } => {
                let since = self.fresh().ino;
                let facts = self.units.entry(unit.clone()).or_default();
                if facts.running.is_some() {
                    facts.running = facts.loaded.clone();
                    facts.since = Some(since);
                }
                done(Vec::new())
            }
            Action::InstallRuntime { .. } => self.install_runtime(),
            Action::RemoveRuntime { created, .. } => {
                for path in created {
                    for entry in self.subtree(path) {
                        self.files.remove(&entry);
                    }
                }
                done(Vec::new())
            }
            Action::ActivateProfile { user, .. } => {
                if self.contents(DEFAULT_PROFILE_NIX_ENV).is_none() {
                    return Err(Failure::SpawnFailed {
                        program: DEFAULT_PROFILE_NIX_ENV.to_string(),
                        kind: std::io::ErrorKind::NotFound,
                    });
                }
                let state = crate::declared::paths::mix_state_dir(&user.home);
                let config: Vec<Option<Arc<[u8]>>> = [
                    crate::declared::paths::FLAKE_NIX,
                    crate::declared::paths::HOME_NIX,
                    crate::declared::paths::FLAKE_LOCK,
                    crate::declared::paths::STATE_FILE,
                ]
                .iter()
                .map(|file| self.contents(state.join(file)).map(Arc::from))
                .collect();
                let profile = self.profiles.entry(user.uid).or_default();
                let previous = profile.active;
                let last = profile.generations.iter().max().copied();
                let built = profile
                    .generations
                    .iter()
                    .copied()
                    .filter(|generation| !profile.dangling.contains(generation))
                    .filter(|generation| profile.built.get(generation) == Some(&config))
                    .max();
                if let Some(built) = built {
                    profile.active = Some(built);
                    return done(if previous == Some(built) {
                        Vec::new()
                    } else {
                        vec![
                            Action::SwitchGeneration {
                                user: user.clone(),
                                generation: previous,
                                expect: Some(built),
                            },
                            Action::ApplyGeneration { user: user.clone() },
                        ]
                    });
                }
                let generation = last.map_or(1, |last| last + 1);
                profile.generations.push(generation);
                profile.built.insert(generation, config);
                profile.active = Some(generation);
                done(vec![
                    Action::SwitchGeneration {
                        user: user.clone(),
                        generation: previous,
                        expect: Some(generation),
                    },
                    Action::DeleteGeneration {
                        user: user.clone(),
                        generation,
                    },
                    Action::ApplyGeneration { user: user.clone() },
                ])
            }
            Action::DeleteGeneration { user, generation } => {
                let profile = self.profiles.entry(user.uid).or_default();
                if profile.active == Some(*generation) {
                    return Err(account_conflict(
                        &user.name,
                        format!("generation {generation} inactive"),
                        "the active generation",
                    ));
                }
                if !profile.generations.contains(generation) {
                    return Err(account_conflict(
                        &user.name,
                        format!("generation {generation}"),
                        "no such generation",
                    ));
                }
                profile.generations.retain(|kept| kept != generation);
                profile.dangling.retain(|kept| kept != generation);
                profile.built.remove(generation);
                if profile.generations.is_empty() && profile.active.is_none() {
                    self.profiles.remove(&user.uid);
                }
                done(Vec::new())
            }
            Action::SwitchGeneration {
                user,
                generation,
                expect,
            } => {
                let profile = self.profiles.entry(user.uid).or_default();
                if profile.active != *expect {
                    return Err(account_conflict(
                        &user.name,
                        format!("generation {expect:?}"),
                        format!("generation {:?}", profile.active),
                    ));
                }
                profile.active = *generation;
                done(vec![Action::SwitchGeneration {
                    user: user.clone(),
                    generation: *expect,
                    expect: *generation,
                }])
            }
            Action::ApplyGeneration { user } => {
                done(vec![Action::ApplyGeneration { user: user.clone() }])
            }
            Action::CollectGarbage { .. } => done(Vec::new()),
            Action::RecordState { user } => done(self.record_state(user)),
            Action::CreateRepository { user } => done(self.create_repository(user)?),
            Action::Commit => {
                let committed = self.pending.remove(&self.acting_for).unwrap_or_default();
                for pending in committed {
                    for path in self.subtree(&pending) {
                        self.files.remove(&path);
                    }
                }
                done(Vec::new())
            }
        }
    }

    fn already(&self, action: &Action) -> bool {
        match action {
            Action::EnableUnit { unit } => self.units.get(unit).is_some_and(|unit| unit.enabled),
            Action::DisableUnit { unit } => !self.units.get(unit).is_some_and(|unit| unit.enabled),
            Action::StartUnit { unit } => self
                .units
                .get(unit)
                .is_some_and(|unit| unit.running.is_some()),
            Action::StopUnit { unit } => !self
                .units
                .get(unit)
                .is_some_and(|unit| unit.running.is_some()),
            _ => false,
        }
    }

    fn tree_owner(&self, path: &Path) -> Option<Owner> {
        path.ancestors()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .filter_map(|ancestor| self.files.get(ancestor))
            .map(|entry| entry.owner)
            .find(|owner| owner.0 != 0)
    }

    fn running_for(&self, path: &Path) -> Owner {
        self.runs_as
            .unwrap_or_else(|| self.tree_owner(path).unwrap_or(ROOT))
    }

    fn reload(&mut self) {
        let names: Vec<String> = self
            .units
            .keys()
            .cloned()
            .chain(self.unit_files())
            .collect();
        for name in names {
            let loaded = self.contents(unit_path(&name)).map(Arc::from);
            let stamp = self.stamp(unit_path(&name));
            let facts = self.units.entry(name).or_default();
            if facts.held() {
                facts.loaded = loaded;
                facts.stamp = stamp;
            }
        }
    }

    fn loaded_unit(&mut self, unit: &str, operation: UnitOperation) -> Result<&mut Unit, Failure> {
        let file = self.contents(unit_path(unit)).map(Arc::from);
        let stamp = self.stamp(unit_path(unit));
        let facts = self.units.entry(unit.to_string()).or_default();
        if !facts.held() {
            facts.loaded = file;
            facts.stamp = stamp;
        }
        if facts.loaded.is_none() {
            return Err(Failure::Unit(Box::new(UnitFailure {
                operation,
                unit: unit.to_string(),
                job_result: "failed".to_string(),
                active_state: "inactive".to_string(),
                sub_state: "dead".to_string(),
                unit_result: "not-found".to_string(),
                invocation: None,
            })));
        }
        Ok(facts)
    }

    fn unit_files(&self) -> Vec<String> {
        let dir = Path::new(UNIT_DIR);
        self.files
            .keys()
            .filter(|path| path.parent() == Some(dir))
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect()
    }

    fn install_runtime(&mut self) -> Outcome {
        let files: [(&str, &[u8]); 3] = [
            (DEFAULT_PROFILE_NIX_ENV, b"nix-env"),
            (NIX_DAEMON_SERVICE_SRC, b"[Service]\nExecStart=nix-daemon\n"),
            (
                NIX_DAEMON_SOCKET_SRC,
                b"[Socket]\nListenStream=/nix/var/nix/daemon-socket/socket\n",
            ),
        ];
        let mut created = Vec::new();
        let store = Path::new(crate::declared::paths::NIX_STORE);
        if !self.files.contains_key(store) {
            self.parent_is_dir(store)?;
            let id = self.fresh();
            self.files.insert(
                store.to_path_buf(),
                Entry {
                    content: Content::Directory,
                    mode: 0o1775,
                    owner: ROOT,
                    id,
                    changed: id.ino,
                },
            );
            created.push(store.to_path_buf());
        }
        for (file, contents) in files {
            let file = Path::new(file);
            let mut missing: Vec<&Path> = file
                .ancestors()
                .skip(1)
                .take_while(|dir| !self.files.contains_key(*dir))
                .collect();
            missing.reverse();
            if let Some(top) = missing.first() {
                created.push(top.to_path_buf());
            }
            for dir in missing {
                self.parent_is_dir(dir)?;
                let id = self.fresh();
                self.files.insert(
                    dir.to_path_buf(),
                    Entry {
                        content: Content::Directory,
                        mode: 0o755,
                        owner: ROOT,
                        id,
                        changed: id.ino,
                    },
                );
            }
            self.put(file, &Arc::from(contents), 0o444, ROOT, Expect::Absent)?;
        }
        done(vec![Action::RemoveRuntime {
            created,
            kept: Vec::new(),
        }])
    }

    pub fn seed_path(&mut self, path: &Path, facts: &PathFacts, contents: Option<Arc<[u8]>>) {
        let content = match facts.kind {
            Kind::Missing => {
                self.files.remove(path);
                return;
            }
            Kind::Directory => Content::Directory,
            Kind::File | Kind::Symlink | Kind::Other | Kind::Unreadable(_) => {
                Content::File(contents.unwrap_or_else(|| Arc::from(&[][..])))
            }
        };
        let id = facts.id.unwrap_or_else(|| self.fresh());
        self.files.insert(
            path.to_path_buf(),
            Entry {
                content,
                mode: facts.mode,
                owner: facts.owner,
                id,
                changed: facts
                    .changed
                    .map_or(id.ino, |(seconds, _)| u64::try_from(seconds).unwrap_or(0)),
            },
        );
    }

    pub fn seed_group(&mut self, name: &str, found: Option<GroupFacts>) {
        match found {
            Some(group) => {
                self.groups.insert(name.to_string(), group);
            }
            None => {
                self.groups.remove(name);
            }
        }
    }

    pub fn seed_user(&mut self, name: &str, found: Option<UserFacts>) {
        match found {
            Some(user) => {
                self.users.insert(name.to_string(), user);
            }
            None => {
                self.users.remove(name);
            }
        }
    }

    pub fn seed_unit(&mut self, name: &str, found: &UnitFacts, file: Option<Arc<[u8]>>) {
        let loaded = (found.load_state == "loaded").then_some(file).flatten();
        let running = matches!(found.active_state.as_str(), "active" | "reloading")
            .then(|| loaded.clone())
            .flatten();
        let since = running.is_some().then(|| self.fresh().ino);
        let stamp = self.stamp(unit_path(name)).filter(|_| !found.needs_reload);
        self.units.insert(
            name.to_string(),
            Unit {
                loaded,
                enabled: found.enabled(),
                running,
                since,
                stamp,
            },
        );
    }

    pub fn seed_profile(&mut self, uid: u32, found: &ProfileFacts) {
        self.profiles.insert(
            uid,
            Profile {
                generations: found.generations.clone(),
                active: found.active,
                built: BTreeMap::new(),
                dangling: found.dangling.clone(),
            },
        );
    }

    pub fn observe(&self, query: &Query) -> Fact {
        match query {
            Query::Path(path) => Fact::Path(match self.files.get(path) {
                None => PathFacts {
                    kind: Kind::Missing,
                    mode: 0,
                    owner: ROOT,
                    id: None,
                    digest: None,
                    changed: None,
                },
                Some(entry) => PathFacts {
                    kind: match entry.content {
                        Content::Directory => Kind::Directory,
                        Content::File(_) => Kind::File,
                    },
                    mode: entry.mode,
                    owner: entry.owner,
                    id: Some(entry.id),
                    digest: None,
                    changed: Some((entry.changed as i64, 0)),
                },
            }),
            Query::Contents(path) => Fact::Contents(self.contents(path).map(Arc::from)),
            Query::Group(name) => Fact::Group(self.groups.get(name).cloned()),
            Query::User(name) => Fact::User(self.users.get(name).cloned()),
            Query::TreeOwner(path) => Fact::TreeOwner(self.tree_owner(path).map(|owner| owner.0)),
            Query::Repository(user) => Fact::Repository {
                intact: self.usable(user) && self.verifies(user) && self.reusable(user),
                recorded: self.usable(user)
                    && self.verifies(user)
                    && self.reusable(user)
                    && self.indexed(user)
                    && self
                        .committed(user)
                        .is_some_and(|recorded| recorded == self.staged(user)),
            },
            Query::Profile(user) => Fact::Profile(
                self.profiles
                    .get(&user.uid)
                    .map(|profile| {
                        let mut generations = profile.generations.clone();
                        generations.sort_unstable();
                        let mut dangling = profile.dangling.clone();
                        dangling.sort_unstable();
                        ProfileFacts {
                            generations,
                            active: profile.active,
                            dangling,
                        }
                    })
                    .unwrap_or_default(),
            ),
            Query::ActiveList(user) => Fact::Contents(self.active_list(user).map(Arc::from)),
            Query::Journals(_) => Fact::Journals(
                self.journals
                    .iter()
                    .cloned()
                    .chain(
                        self.logs
                            .iter()
                            .filter(|(request, _)| !self.held.contains(*request))
                            .map(|(request, records)| {
                                crate::run::journal::abandoned(request, records)
                            }),
                    )
                    .collect(),
            ),
            Query::Leftovers(dir) => Fact::Leftovers(
                self.files
                    .iter()
                    .filter(|(path, _)| {
                        path.parent() == Some(dir.as_path())
                            && path
                                .file_name()
                                .is_some_and(|name| is_leftover(&name.to_string_lossy()))
                            && !self
                                .pending
                                .get(&self.acting_for)
                                .is_some_and(|own| own.contains(path))
                    })
                    .map(|(path, entry)| (path.clone(), entry.id))
                    .collect(),
            ),
            Query::Strangers { path, owner } => Fact::Stranger(
                self.subtree(path)
                    .into_iter()
                    .map(|at| {
                        let found = self.files[&at].owner;
                        (at, found)
                    })
                    .find(|(_, found)| found != owner),
            ),
            Query::Program { path, source } => {
                let installed = self.contents(path);
                let running = self.contents(source);
                let same = installed.is_some() && installed == running;
                Fact::Program(crate::effect::ProgramFacts {
                    same,
                    source: if same { None } else { running.map(Arc::from) },
                })
            }
            Query::Clobbered(user) => {
                Fact::Clobbered(self.clobbered.get(&user.uid).cloned().unwrap_or_default())
            }
            Query::Unit(name) => {
                let facts = self.units.get(name).cloned().unwrap_or_default();
                let file = self.contents(unit_path(name)).map(Arc::from);
                let loaded = facts.seen(file.clone());
                let stale = facts.held()
                    && match (self.stamp(unit_path(name)), facts.stamp) {
                        (Some(stamp), Some(loaded)) => stamp > loaded,
                        (Some(_), None) | (None, Some(_)) => true,
                        (None, None) => false,
                    };
                Fact::Unit(UnitFacts {
                    load_state: if loaded.is_some() {
                        "loaded"
                    } else {
                        "not-found"
                    }
                    .to_string(),
                    active_state: if facts.running.is_some() {
                        "active"
                    } else {
                        "inactive"
                    }
                    .to_string(),
                    file_state: match (facts.enabled, &file) {
                        (true, _) => "enabled",
                        (false, Some(_)) => "disabled",
                        (false, None) => "",
                    }
                    .to_string(),
                    needs_reload: stale,
                    active_since: facts.since.map(|since| (since as i64, 0)),
                })
            }
        }
    }

    pub fn active_list(&self, user: &InvokingUser) -> Option<&[u8]> {
        let profile = self.profiles.get(&user.uid)?;
        profile.built.get(&profile.active?)?.get(3)?.as_deref()
    }

    pub fn profile(&self, user: &InvokingUser) -> Option<&Profile> {
        self.profiles.get(&user.uid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::UserSpec;
    use crate::run::journal::rollback_order;

    fn bytes(text: &str) -> Arc<[u8]> {
        Arc::from(text.as_bytes())
    }

    fn run(world: &mut World, actions: &[Action]) -> Vec<Vec<Action>> {
        actions
            .iter()
            .map(|action| world.apply(action).expect("the action applies").undo)
            .collect()
    }

    fn roll_back(world: &mut World, journal: &[Vec<Action>]) {
        for undo in rollback_order(journal) {
            world.apply(&undo).expect("the undo applies");
        }
    }

    fn id_of(world: &World, path: &str) -> FileId {
        world.files[Path::new(path)].id
    }

    #[test]
    fn a_tree_root_took_over_is_copied_back_to_its_owner_and_can_be_undone() {
        let alice = (1000, 1000);
        let mut world = World::default();
        world
            .with_dir("/home/alice", 0o700, alice)
            .with_dir("/home/alice/state", 0o755, ROOT)
            .with_file("/home/alice/state/flake.nix", b"{}", 0o4644, ROOT);
        let before = world.clone();
        let expect = id_of(&world, "/home/alice/state");

        let journal = run(
            &mut world,
            &[Action::ReclaimTree {
                path: "/home/alice/state".into(),
                expect,
                owner: alice,
                mode: 0o700,
            }],
        );

        let state = &world.files[Path::new("/home/alice/state")];
        let flake = &world.files[Path::new("/home/alice/state/flake.nix")];
        assert_eq!((state.owner, state.mode), (alice, 0o700));
        assert_eq!((flake.owner, flake.mode), (alice, 0o644));
        assert_eq!(flake.content, Content::File(Arc::from(&b"{}"[..])));
        roll_back(&mut world, &journal);
        assert_eq!(world, before);
    }

    #[test]
    fn creating_a_tree_and_rolling_it_back_leaves_the_world_as_it_was() {
        let mut world = World::default();
        let before = world.clone();

        let journal = run(
            &mut world,
            &[
                Action::CreateDir {
                    path: "/nix".into(),
                    mode: 0o755,
                    owner: None,
                },
                Action::CreateDir {
                    path: "/nix/var".into(),
                    mode: 0o755,
                    owner: None,
                },
                Action::PutFile {
                    path: "/nix/.mix-managed".into(),
                    contents: bytes(""),
                    mode: 0o644,
                    owner: None,
                    expect: Expect::Absent,
                },
            ],
        );
        roll_back(&mut world, &journal);

        assert_eq!(world, before);
    }

    #[test]
    fn replacing_a_file_keeps_the_old_one_until_commit_and_undo_brings_it_back() {
        let mut world = World::default();
        world.with_dir("/etc/nix", 0o755, ROOT).with_file(
            "/etc/nix/nix.conf",
            b"trusted-users = root alice\n",
            0o644,
            ROOT,
        );
        let before = world.clone();
        let old = id_of(&world, "/etc/nix/nix.conf");

        let journal = run(
            &mut world,
            &[Action::PutFile {
                path: "/etc/nix/nix.conf".into(),
                contents: bytes("trusted-users = root\n"),
                mode: 0o644,
                owner: None,
                expect: Expect::Present(old),
            }],
        );
        assert_eq!(world.pending().len(), 1);
        roll_back(&mut world, &journal);

        assert_eq!(world, before);
        assert_eq!(id_of(&world, "/etc/nix/nix.conf"), old);
    }

    #[test]
    fn a_commit_deletes_what_was_kept_for_undo() {
        let mut world = World::default();
        world
            .with_dir("/nix", 0o755, ROOT)
            .with_file("/nix/old", b"x", 0o644, ROOT);
        let id = id_of(&world, "/nix");

        world
            .apply(&Action::SetAside {
                path: "/nix".into(),
                expect: id,
            })
            .unwrap();
        world.apply(&Action::Commit).unwrap();

        assert!(world.pending().is_empty());
        assert!(!world.files.keys().any(|path| path.starts_with("/.nix")));
        assert!(!world.files.contains_key(Path::new("/nix")));
    }

    #[test]
    fn setting_aside_a_whole_tree_is_undone_with_every_entry_in_place() {
        let mut world = World::default();
        world
            .with_dir("/nix", 0o755, ROOT)
            .with_dir("/nix/store", 0o1775, ROOT)
            .with_file("/nix/store/x", b"x", 0o444, ROOT);
        let before = world.clone();
        let id = id_of(&world, "/nix");

        let journal = run(
            &mut world,
            &[Action::SetAside {
                path: "/nix".into(),
                expect: id,
            }],
        );
        roll_back(&mut world, &journal);

        assert_eq!(world, before);
    }

    #[test]
    fn an_action_on_stale_facts_is_refused() {
        let mut world = World::default();
        world.with_file("/etc/foreign", b"theirs", 0o644, ROOT);

        let refused = world.apply(&Action::PutFile {
            path: "/etc/foreign".into(),
            contents: bytes("ours"),
            mode: 0o644,
            owner: None,
            expect: Expect::Absent,
        });

        assert!(matches!(refused, Err(Failure::Conflict { .. })));
        assert_eq!(world.contents("/etc/foreign"), Some(&b"theirs"[..]));
    }

    #[test]
    fn an_undo_never_removes_what_someone_else_put_there() {
        let mut world = World::default();
        let journal = run(
            &mut world,
            &[Action::CreateDir {
                path: "/nix".into(),
                mode: 0o755,
                owner: None,
            }],
        );
        world.with_file("/nix/theirs", b"x", 0o644, ROOT);

        let undo = &rollback_order(&journal)[0];

        assert!(matches!(world.apply(undo), Err(Failure::Conflict { .. })));
        assert!(world.files.contains_key(Path::new("/nix/theirs")));
    }

    #[test]
    fn a_replaced_file_that_changed_again_is_not_clobbered_by_undo() {
        let mut world = World::default();
        world.with_file("/etc/profile.d.conf", b"old", 0o644, ROOT);
        let old = id_of(&world, "/etc/profile.d.conf");
        let journal = run(
            &mut world,
            &[Action::PutFile {
                path: "/etc/profile.d.conf".into(),
                contents: bytes("ours"),
                mode: 0o644,
                owner: None,
                expect: Expect::Present(old),
            }],
        );
        world.with_file("/etc/profile.d.conf", b"edited meanwhile", 0o644, ROOT);

        let undo = &rollback_order(&journal)[0];

        assert!(matches!(world.apply(undo), Err(Failure::Conflict { .. })));
        assert_eq!(
            world.contents("/etc/profile.d.conf"),
            Some(&b"edited meanwhile"[..])
        );
    }

    #[test]
    fn units_are_rolled_back_with_the_reload_after_the_files_are_restored() {
        let mut world = World::default();
        let before = world.clone();

        let journal = run(
            &mut world,
            &[
                Action::InstallUnit {
                    unit: "nix-daemon.service".into(),
                    contents: bytes("[Service]"),
                    expect: Expect::Absent,
                },
                Action::InstallUnit {
                    unit: "nix-daemon.socket".into(),
                    contents: bytes("[Socket]"),
                    expect: Expect::Absent,
                },
                Action::DaemonReload,
                Action::EnableUnit {
                    unit: "nix-daemon.socket".into(),
                },
                Action::StartUnit {
                    unit: "nix-daemon.socket".into(),
                },
            ],
        );
        let Fact::Unit(unit) = world.observe(&Query::Unit("nix-daemon.socket".into())) else {
            panic!("a unit query is answered with unit facts");
        };
        assert_eq!(unit.load_state, "loaded");
        assert_eq!(unit.active_state, "active");
        assert!(unit.enabled());
        assert!(!unit.needs_reload);
        roll_back(&mut world, &journal);

        let unloaded = |world: &World| {
            world
                .units
                .values()
                .all(|unit| unit.loaded.is_none() && unit.running.is_none() && !unit.enabled)
        };
        assert!(unloaded(&world));
        assert_eq!(world.files, before.files);
    }

    #[test]
    fn accounts_come_back_with_their_ids_and_members() {
        let mut world = World::default();
        world.groups.insert(
            "nixbld".into(),
            GroupFacts {
                gid: 30_000,
                members: vec!["nixbld1".into()],
            },
        );
        world.users.insert(
            "nixbld1".into(),
            UserFacts {
                uid: 30_001,
                gid: 30_000,
                home: "/var/empty".into(),
                shell: "/usr/sbin/nologin".into(),
                comment: "mix build user 1".into(),
            },
        );
        let before = world.clone();

        let journal = run(
            &mut world,
            &[
                Action::DeleteUser {
                    name: "nixbld1".into(),
                    expect: (30_001, 30_000),
                    comment: "mix build user 1".into(),
                },
                Action::DeleteGroup {
                    name: "nixbld".into(),
                    expect: 30_000,
                },
            ],
        );
        roll_back(&mut world, &journal);

        assert_eq!(world, before);
    }

    #[test]
    fn an_undo_leaves_a_same_named_account_someone_else_made() {
        let mut world = World::default();
        world
            .apply(&Action::AddGroup {
                name: "nixbld".into(),
                gid: 30_000,
            })
            .unwrap();
        let spec = UserSpec {
            name: "nixbld1".into(),
            uid: 30_001,
            gid: 30_000,
            home: "/var/empty".into(),
            shell: "/usr/sbin/nologin".into(),
            comment: "mix build user 1 for request a".into(),
            groups: vec![],
        };
        let undo = world.apply(&Action::AddUser(spec.clone())).unwrap().undo;
        world.apply(&undo[0].clone()).unwrap();
        world
            .apply(&Action::AddUser(UserSpec {
                comment: "Nix build user 1".into(),
                ..spec
            }))
            .unwrap();

        let refused = world.apply(&undo[0]);

        assert!(matches!(refused, Err(Failure::Conflict { .. })));
        assert!(world.users.contains_key("nixbld1"));
    }

    #[test]
    fn a_taken_uid_is_a_conflict() {
        let mut world = World::default();
        world.users.insert(
            "someone".into(),
            UserFacts {
                uid: 30_001,
                gid: 100,
                home: "/home/someone".into(),
                shell: "/bin/sh".into(),
                comment: String::new(),
            },
        );

        let refused = world.apply(&Action::AddUser(UserSpec {
            name: "nixbld1".into(),
            uid: 30_001,
            gid: 30_000,
            home: "/var/empty".into(),
            shell: "/usr/sbin/nologin".into(),
            comment: String::new(),
            groups: vec![],
        }));

        assert!(matches!(refused, Err(Failure::Conflict { .. })));
    }

    #[test]
    fn activating_an_unchanged_config_reuses_the_last_generation() {
        let mut world = World::default();
        world
            .with_dir("/nix", 0o755, ROOT)
            .with_dir("/nix/var", 0o755, ROOT)
            .with_dir("/nix/var/nix", 0o755, ROOT)
            .with_dir("/nix/var/nix/profiles", 0o755, ROOT);
        let user = InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".into(),
            home: "/home/alice".into(),
        };
        let activate = Action::ActivateProfile {
            user: user.clone(),
            source: crate::effect::FlakeSource::Git,
        };
        run(
            &mut world,
            &[Action::InstallRuntime {
                url: "https://example.invalid/nix.tar.xz".into(),
                sha256: crate::effect::Digest([0; 32]),
                size: 1,
            }],
        );
        world.apply(&activate).unwrap();
        let once = world.clone();

        let again = world.apply(&activate).unwrap();

        assert!(again.undo.is_empty());
        assert_eq!(world, once);
        world
            .apply(&Action::SwitchGeneration {
                user: user.clone(),
                generation: None,
                expect: Some(1),
            })
            .unwrap();
        let back = world.apply(&activate).unwrap();
        assert_eq!(
            back.undo,
            [
                Action::SwitchGeneration {
                    user: user.clone(),
                    generation: None,
                    expect: Some(1),
                },
                Action::ApplyGeneration { user: user.clone() },
            ]
        );
        assert_eq!(
            world.profile(&user).map(|p| p.generations.clone()),
            Some(vec![1])
        );
    }

    #[test]
    fn the_runtime_and_an_activation_are_undone_to_the_previous_generation() {
        let mut world = World::default();
        world
            .with_dir("/nix", 0o755, ROOT)
            .with_dir("/nix/var", 0o755, ROOT)
            .with_dir("/nix/var/nix", 0o755, ROOT)
            .with_dir("/nix/var/nix/profiles", 0o755, ROOT);
        let before = world.clone();
        let user = InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".into(),
            home: "/home/alice".into(),
        };

        let journal = run(
            &mut world,
            &[
                Action::InstallRuntime {
                    url: "https://example.invalid/nix.tar.xz".into(),
                    sha256: crate::effect::Digest([0; 32]),
                    size: 1,
                },
                Action::ActivateProfile {
                    user: user.clone(),
                    source: crate::effect::FlakeSource::Git,
                },
            ],
        );
        assert_eq!(world.profile(&user).and_then(|p| p.active), Some(1));
        roll_back(&mut world, &journal);

        assert_eq!(world.files, before.files);
        assert_eq!(world.profile(&user).and_then(|p| p.active), None);
    }

    #[test]
    fn an_action_whose_result_already_holds_changes_nothing_and_needs_no_undo() {
        let mut world = World::default();
        world.groups.insert(
            "nixbld".into(),
            GroupFacts {
                gid: 30_000,
                members: vec![],
            },
        );
        let before = world.clone();

        for action in [
            Action::AddGroup {
                name: "nixbld".into(),
                gid: 30_000,
            },
            Action::DeleteUser {
                name: "nixbld1".into(),
                expect: (30_001, 30_000),
                comment: String::new(),
            },
            Action::RemoveCreated {
                path: "/nix".into(),
                expect: FileId {
                    dev: 1,
                    ino: 99,
                    born: None,
                },
            },
            Action::StopUnit {
                unit: "nix-daemon.socket".into(),
            },
        ] {
            assert_eq!(
                world.apply(&action),
                Ok(Performed { undo: vec![] }),
                "{action:?}"
            );
        }
        assert_eq!(world, before);
        assert!(matches!(
            world.apply(&Action::AddGroup {
                name: "nixbld".into(),
                gid: 1,
            }),
            Err(Failure::Conflict { .. })
        ));
    }
}
