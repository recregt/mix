use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::action::{
    Action, Expect, Fact, Failure, FileId, GroupFacts, Kind, Outcome, Owner, PathFacts, Performed,
    Query, UnitFacts, UnitFailure, UnitOperation, UserFacts, UserSpec,
};
use crate::paths::{DEFAULT_PROFILE_NIX_ENV, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SOCKET_SRC};
use crate::privilege::InvokingUser;

pub use crate::paths::SYSTEMD_UNIT_DIR as UNIT_DIR;

const ROOT: Owner = (0, 0);

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
}

#[derive(Debug, Clone)]
pub struct World {
    pub files: BTreeMap<PathBuf, Entry>,
    pub groups: BTreeMap<String, GroupFacts>,
    pub users: BTreeMap<String, UserFacts>,
    pub units: BTreeMap<String, Unit>,
    pub profiles: BTreeMap<u32, Profile>,
    pending: Vec<PathBuf>,
    next_ino: u64,
}

impl PartialEq for World {
    fn eq(&self, other: &Self) -> bool {
        self.files == other.files
            && self.groups == other.groups
            && self.users == other.users
            && self.meaningful_units().eq(other.meaningful_units())
            && self.profiles == other.profiles
            && self.pending == other.pending
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

impl Default for World {
    fn default() -> Self {
        let mut world = Self {
            files: BTreeMap::new(),
            groups: BTreeMap::new(),
            users: BTreeMap::new(),
            units: BTreeMap::new(),
            profiles: BTreeMap::new(),
            pending: Vec::new(),
            next_ino: 1,
        };
        for dir in [
            "/",
            "/etc",
            "/etc/systemd",
            UNIT_DIR,
            "/home",
            "/var",
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

    pub fn now(&self) -> u64 {
        self.next_ino
    }

    pub fn pending(&self) -> &[PathBuf] {
        &self.pending
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
        match path.parent().and_then(|parent| self.files.get(parent)) {
            Some(Entry {
                content: Content::Directory,
                ..
            }) => Ok(()),
            _ => Err(not_found(path)),
        }
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
        path.with_file_name(format!(".{name}.mix-{purpose}-{n}"))
    }

    fn matches(&self, path: &Path, expect: Expect) -> Result<(), Failure> {
        let found = self.files.get(path);
        match (expect, found) {
            (Expect::Absent, None) => Ok(()),
            (Expect::Present(id), Some(entry)) if entry.id == id => Ok(()),
            (Expect::Absent, found) => Err(conflict(path, "nothing", describe(found))),
            (Expect::Present(id), found) => Err(conflict(path, format!("{id:?}"), describe(found))),
        }
    }

    fn put(
        &mut self,
        path: &Path,
        contents: &Arc<[u8]>,
        mode: u32,
        owner: Owner,
        expect: Expect,
    ) -> Result<Vec<Action>, Failure> {
        self.parent_is_dir(path)?;
        self.matches(path, expect)?;
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
                self.pending.push(backup.clone());
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
        match action {
            Action::CreateDir { path, mode, owner } => {
                self.parent_is_dir(path)?;
                self.matches(path, Expect::Absent)?;
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
            Action::PutFile {
                path,
                contents,
                mode,
                owner,
                expect,
            } => done(self.put(path, contents, *mode, owner.unwrap_or(ROOT), *expect)?),
            Action::SetMode { path, mode, expect } => {
                let entry = self.files.get_mut(path).ok_or_else(|| not_found(path))?;
                if entry.mode != *expect {
                    return Err(conflict(
                        path,
                        format!("mode {expect:o}"),
                        format!("mode {:o}", entry.mode),
                    ));
                }
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
                if entry.owner != *expect {
                    return Err(conflict(
                        path,
                        format!("owner {expect:?}"),
                        format!("owner {:?}", entry.owner),
                    ));
                }
                entry.owner = *owner;
                done(vec![Action::SetOwner {
                    path: path.clone(),
                    owner: *expect,
                    expect: *owner,
                }])
            }
            Action::SetAside { path, expect } => {
                self.matches(path, Expect::Present(*expect))?;
                let aside = self.sibling(path, "aside");
                self.move_tree(path, &aside);
                self.pending.push(aside.clone());
                done(vec![Action::Restore {
                    path: path.clone(),
                    from: aside,
                    expect: Expect::Absent,
                }])
            }
            Action::RemoveCreated { path, expect } => {
                self.matches(path, Expect::Present(*expect))?;
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
            Action::RemoveCreatedTree { path, expect } => {
                self.matches(path, Expect::Present(*expect))?;
                for entry in self.subtree(path) {
                    self.files.remove(&entry);
                }
                done(Vec::new())
            }
            Action::Restore { path, from, expect } => {
                self.matches(path, *expect)?;
                if !self.files.contains_key(from) {
                    return Err(not_found(from));
                }
                for discarded in self.subtree(path) {
                    self.files.remove(&discarded);
                }
                self.move_tree(from, path);
                self.pending.retain(|pending| pending != from);
                done(Vec::new())
            }
            Action::AddGroup { name, gid } => {
                if let Some(found) = self.groups.get(name) {
                    return Err(account_conflict(
                        name,
                        "no group",
                        format!("gid {}", found.gid),
                    ));
                }
                self.groups.insert(
                    name.clone(),
                    GroupFacts {
                        gid: *gid,
                        members: Vec::new(),
                    },
                );
                done(vec![Action::DeleteGroup {
                    name: name.clone(),
                    expect: *gid,
                }])
            }
            Action::SetGroupGid { name, gid, expect } => {
                let group = self.group(name, *expect)?;
                group.gid = *gid;
                done(vec![Action::SetGroupGid {
                    name: name.clone(),
                    gid: *expect,
                    expect: *gid,
                }])
            }
            Action::DeleteGroup { name, expect } => {
                let members = self.group(name, *expect)?.members.clone();
                self.groups.remove(name);
                let mut undo = vec![Action::AddGroup {
                    name: name.clone(),
                    gid: *expect,
                }];
                undo.extend(members.into_iter().map(|user| Action::AddMember {
                    group: name.clone(),
                    user,
                }));
                done(undo)
            }
            Action::AddUser(spec) => {
                if let Some(found) = self.users.get(&spec.name) {
                    return Err(account_conflict(
                        &spec.name,
                        "no user",
                        format!("uid {}", found.uid),
                    ));
                }
                if let Some((holder, _)) = self.users.iter().find(|(_, user)| user.uid == spec.uid)
                {
                    return Err(account_conflict(
                        &spec.name,
                        format!("uid {} free", spec.uid),
                        format!("uid {} held by {holder}", spec.uid),
                    ));
                }
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
                done(vec![Action::DeleteUser {
                    name: spec.name.clone(),
                    expect: (spec.uid, spec.gid),
                    comment: spec.comment.clone(),
                }])
            }
            Action::SetUserIds { name, ids, expect } => {
                let user = self.user(name, *expect)?;
                (user.uid, user.gid) = *ids;
                done(vec![Action::SetUserIds {
                    name: name.clone(),
                    ids: *expect,
                    expect: *ids,
                }])
            }
            Action::DeleteUser {
                name,
                expect,
                comment,
            } => {
                let user = self.user(name, *expect)?.clone();
                if user.comment != *comment {
                    return Err(account_conflict(
                        name,
                        format!("comment {comment:?}"),
                        format!("comment {:?}", user.comment),
                    ));
                }
                self.users.remove(name);
                let groups: Vec<String> = self
                    .groups
                    .iter_mut()
                    .filter_map(|(group, facts)| {
                        let before = facts.members.len();
                        facts.members.retain(|member| member != name);
                        (facts.members.len() != before).then(|| group.clone())
                    })
                    .collect();
                done(vec![Action::AddUser(UserSpec {
                    name: name.clone(),
                    uid: user.uid,
                    gid: user.gid,
                    home: user.home,
                    shell: user.shell,
                    comment: user.comment,
                    groups,
                })])
            }
            Action::AddMember { group, user } => {
                let facts = self
                    .groups
                    .get_mut(group)
                    .ok_or_else(|| account_conflict(group, "a group", "no group"))?;
                if facts.members.contains(user) {
                    return Err(account_conflict(
                        group,
                        format!("{user} absent"),
                        format!("{user} present"),
                    ));
                }
                facts.members.push(user.clone());
                facts.members.sort();
                done(vec![Action::RemoveMember {
                    group: group.clone(),
                    user: user.clone(),
                }])
            }
            Action::RemoveMember { group, user } => {
                let facts = self
                    .groups
                    .get_mut(group)
                    .ok_or_else(|| account_conflict(group, "a group", "no group"))?;
                if !facts.members.contains(user) {
                    return Err(account_conflict(
                        group,
                        format!("{user} present"),
                        format!("{user} absent"),
                    ));
                }
                facts.members.retain(|member| member != user);
                done(vec![Action::AddMember {
                    group: group.clone(),
                    user: user.clone(),
                }])
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
                let names: Vec<String> = self
                    .units
                    .keys()
                    .cloned()
                    .chain(self.unit_files())
                    .collect();
                for name in names {
                    let loaded = self.contents(unit_path(&name)).map(Arc::from);
                    self.units.entry(name).or_default().loaded = loaded;
                }
                done(vec![Action::DaemonReload])
            }
            Action::EnableUnit { unit } => {
                let facts = self.loaded_unit(unit, UnitOperation::Enable)?;
                if facts.enabled {
                    return Err(account_conflict(unit, "disabled", "enabled"));
                }
                facts.enabled = true;
                done(vec![Action::DisableUnit { unit: unit.clone() }])
            }
            Action::DisableUnit { unit } => {
                let facts = self.units.entry(unit.clone()).or_default();
                if !facts.enabled {
                    return Err(account_conflict(unit, "enabled", "disabled"));
                }
                facts.enabled = false;
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
                done(vec![Action::StartUnit { unit: unit.clone() }])
            }
            Action::RestartUnit { unit } => {
                let since = self.fresh().ino;
                let facts = self.units.entry(unit.clone()).or_default();
                if facts.running.is_some() {
                    facts.running = facts.loaded.clone();
                    facts.since = Some(since);
                }
                done(vec![Action::RestartUnit { unit: unit.clone() }])
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
                let profile = self.profiles.entry(user.uid).or_default();
                let previous = profile.active;
                let generation = profile.generations.iter().max().map_or(1, |last| last + 1);
                profile.generations.push(generation);
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
            Action::RecordState { user } => {
                let git = crate::paths::mix_state_dir(&user.home).join(".git");
                if self.files.contains_key(&git) {
                    return done(Vec::new());
                }
                self.parent_is_dir(&git)?;
                let id = self.fresh();
                self.files.insert(
                    git.clone(),
                    Entry {
                        content: Content::Directory,
                        mode: 0o755,
                        owner: (user.uid, user.gid),
                        id,
                        changed: id.ino,
                    },
                );
                done(vec![Action::RemoveCreatedTree {
                    path: git,
                    expect: id,
                }])
            }
            Action::Commit => {
                for pending in std::mem::take(&mut self.pending) {
                    for path in self.subtree(&pending) {
                        self.files.remove(&path);
                    }
                }
                done(Vec::new())
            }
        }
    }

    fn already(&self, action: &Action) -> bool {
        let members = |name: &str| {
            self.groups
                .get(name)
                .map(|group| group.members.clone())
                .unwrap_or_default()
        };
        match action {
            Action::RemoveCreated { path, .. } | Action::RemoveCreatedTree { path, .. } => {
                !self.files.contains_key(path)
            }
            Action::AddGroup { name, gid } | Action::SetGroupGid { name, gid, .. } => {
                self.groups.get(name).is_some_and(|group| group.gid == *gid)
            }
            Action::DeleteGroup { name, .. } => !self.groups.contains_key(name),
            Action::AddUser(spec) => self
                .users
                .get(&spec.name)
                .is_some_and(|user| (user.uid, user.gid) == (spec.uid, spec.gid)),
            Action::SetUserIds { name, ids, .. } => self
                .users
                .get(name)
                .is_some_and(|user| (user.uid, user.gid) == *ids),
            Action::DeleteUser { name, .. } => !self.users.contains_key(name),
            Action::AddMember { group, user } => members(group).contains(user),
            Action::RemoveMember { group, user } => !members(group).contains(user),
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

    fn group(&mut self, name: &str, expect: u32) -> Result<&mut GroupFacts, Failure> {
        match self.groups.get_mut(name) {
            Some(group) if group.gid == expect => Ok(group),
            Some(group) => Err(account_conflict(
                name,
                format!("gid {expect}"),
                format!("gid {}", group.gid),
            )),
            None => Err(account_conflict(name, format!("gid {expect}"), "no group")),
        }
    }

    fn user(&mut self, name: &str, expect: Owner) -> Result<&mut UserFacts, Failure> {
        match self.users.get_mut(name) {
            Some(user) if (user.uid, user.gid) == expect => Ok(user),
            Some(user) => Err(account_conflict(
                name,
                format!("ids {expect:?}"),
                format!("ids {:?}", (user.uid, user.gid)),
            )),
            None => Err(account_conflict(name, format!("ids {expect:?}"), "no user")),
        }
    }

    fn loaded_unit(&mut self, unit: &str, operation: UnitOperation) -> Result<&mut Unit, Failure> {
        let facts = self.units.entry(unit.to_string()).or_default();
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
            Query::Unit(name) => {
                let facts = self.units.get(name).cloned().unwrap_or_default();
                let file = self.contents(unit_path(name)).map(Arc::from);
                Fact::Unit(UnitFacts {
                    load_state: if facts.loaded.is_some() {
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
                    file_state: if facts.enabled { "enabled" } else { "disabled" }.to_string(),
                    needs_reload: facts.loaded != file,
                    active_since: facts.since.map(|since| (since as i64, 0)),
                })
            }
        }
    }

    pub fn profile(&self, user: &InvokingUser) -> Option<&Profile> {
        self.profiles.get(&user.uid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::rollback_order;

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
                    sha256: crate::action::Digest([0; 32]),
                    size: 1,
                },
                Action::ActivateProfile {
                    user: user.clone(),
                    allow_source_builds: true,
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
