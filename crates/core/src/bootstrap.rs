use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::action::{
    Action, Digest, Expect, Fact, Failure, GroupFacts, Kind, Owner, PathFacts, Query, UnitFacts,
    UserFacts, UserSpec,
};
use crate::identity::{
    MIX_USERS_GID, MIX_USERS_GROUP, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_HOME, NIXBLD_SHELL,
    NIXBLD_UID_BASE, NIXBLD_USER_COUNT, user_name,
};
use crate::models::{PROFILE_SNIPPET, UserConfig};
use crate::paths::{
    DEFAULT_PROFILE_NIX_ENV, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, MIX_STATE_DIR_MODE, NIX_CONF_DEST,
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_UNIT,
    NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_UNIT, NIX_OWNERSHIP_MARKER,
    NIX_PROFILES_DIR_MODE, NIX_TREE_MODE, NIX_TREE_PATHS, POLICY_FILE, PROFILE_SNIPPET_DEST,
    STATE_FILE, mix_state_dir, nix_profiles_dir,
};
use crate::plan::StepSpec;
use crate::policy::Policy;
use crate::state::StateManifest;

const DIR_MODE: u32 = 0o755;
const FILE_MODE: u32 = 0o644;
const NIXBLD_HOME_MODE: u32 = 0o555;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    pub url: String,
    pub sha256: Digest,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub policy: Policy,
    pub user: Option<UserConfig>,
    pub force: bool,
    pub runtime: Runtime,
    pub request: String,
}

struct Facts<'a>(&'a [Fact]);

impl Facts<'_> {
    fn path(&self, index: usize) -> &PathFacts {
        match &self.0[index] {
            Fact::Path(facts) => facts,
            other => unreachable!("a path query was answered with {other:?}"),
        }
    }

    fn contents(&self, index: usize) -> Option<&[u8]> {
        match &self.0[index] {
            Fact::Contents(contents) => contents.as_deref(),
            other => unreachable!("a contents query was answered with {other:?}"),
        }
    }

    fn group(&self, index: usize) -> Option<&GroupFacts> {
        match &self.0[index] {
            Fact::Group(group) => group.as_ref(),
            other => unreachable!("a group query was answered with {other:?}"),
        }
    }

    fn user(&self, index: usize) -> Option<&UserFacts> {
        match &self.0[index] {
            Fact::User(user) => user.as_ref(),
            other => unreachable!("a user query was answered with {other:?}"),
        }
    }

    fn unit(&self, index: usize) -> &UnitFacts {
        match &self.0[index] {
            Fact::Unit(unit) => unit,
            other => unreachable!("a unit query was answered with {other:?}"),
        }
    }
}

fn refuse(path: &Path, expected: &str, found: Kind) -> Failure {
    Failure::Conflict {
        subject: path.display().to_string(),
        expected: expected.to_string(),
        found: format!("{found:?}").to_lowercase(),
    }
}

pub fn ensure_dir(
    path: &Path,
    facts: &PathFacts,
    mode: u32,
    owner: Option<Owner>,
) -> Result<Vec<Action>, Failure> {
    match facts.kind {
        Kind::Missing => Ok(vec![Action::CreateDir {
            path: path.to_path_buf(),
            mode,
            owner,
        }]),
        Kind::Directory => {
            let mut actions = Vec::new();
            if facts.mode != mode {
                actions.push(Action::SetMode {
                    path: path.to_path_buf(),
                    mode,
                    expect: facts.mode,
                });
            }
            if let Some(owner) = owner
                && facts.owner != owner
            {
                actions.push(Action::SetOwner {
                    path: path.to_path_buf(),
                    owner,
                    expect: facts.owner,
                });
            }
            Ok(actions)
        }
        found => Err(refuse(path, "a directory", found)),
    }
}

fn ensure_exists(
    path: &Path,
    facts: &PathFacts,
    owner: Option<Owner>,
) -> Result<Vec<Action>, Failure> {
    match facts.kind {
        Kind::Missing => Ok(vec![Action::CreateDir {
            path: path.to_path_buf(),
            mode: DIR_MODE,
            owner,
        }]),
        Kind::Directory => Ok(Vec::new()),
        found => Err(refuse(path, "a directory", found)),
    }
}

pub fn ensure_file(
    path: &Path,
    facts: &PathFacts,
    current: Option<&[u8]>,
    wanted: &[u8],
    mode: u32,
    owner: Option<Owner>,
) -> Result<Vec<Action>, Failure> {
    let put = |expect| Action::PutFile {
        path: path.to_path_buf(),
        contents: Arc::from(wanted),
        mode,
        owner,
        expect,
    };
    match facts.kind {
        Kind::Missing => Ok(vec![put(Expect::Absent)]),
        Kind::File if current != Some(wanted) => Ok(vec![put(Expect::Present(
            facts.id.expect("an existing file has an identity"),
        ))]),
        Kind::File => ensure_dir(
            path,
            &PathFacts {
                kind: Kind::Directory,
                ..facts.clone()
            },
            mode,
            owner,
        ),
        found => Err(refuse(path, "a regular file", found)),
    }
}

fn ensure_seeded(
    path: &Path,
    facts: &PathFacts,
    seed: &[u8],
    owner: Option<Owner>,
) -> Result<Vec<Action>, Failure> {
    match facts.kind {
        Kind::Missing => Ok(vec![Action::PutFile {
            path: path.to_path_buf(),
            contents: Arc::from(seed),
            mode: FILE_MODE,
            owner,
            expect: Expect::Absent,
        }]),
        Kind::File => Ok(Vec::new()),
        found => Err(refuse(path, "a regular file", found)),
    }
}

fn set_aside(path: &Path, facts: &PathFacts) -> Option<Action> {
    facts.id.map(|expect| Action::SetAside {
        path: path.to_path_buf(),
        expect,
    })
}

struct RemoveExistingInstallation;

const UNIT_FILES: [&str; 2] = [NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SOCKET_DEST];
const SET_ASIDE: [&str; 4] = [NIX_CONF_DEST, PROFILE_SNIPPET_DEST, POLICY_FILE, "/nix"];

impl StepSpec for RemoveExistingInstallation {
    fn key(&self) -> Cow<'static, str> {
        "remove-existing-installation".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "remove the existing installation".into()
    }

    fn queries(&self) -> Vec<Query> {
        let mut queries = vec![
            Query::Unit(NIX_DAEMON_SOCKET_UNIT.to_string()),
            Query::Unit(NIX_DAEMON_SERVICE_UNIT.to_string()),
        ];
        queries.extend(UNIT_FILES.iter().map(|path| Query::Path(path.into())));
        queries.extend((1..=NIXBLD_USER_COUNT).map(|n| Query::User(user_name(n).into_owned())));
        queries.push(Query::Group(NIXBLD_GROUP.to_string()));
        queries.push(Query::Group(MIX_USERS_GROUP.to_string()));
        queries.extend(SET_ASIDE.iter().map(|path| Query::Path(path.into())));
        queries
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let facts = Facts(facts);
        let mut actions = Vec::new();
        for (index, unit) in [NIX_DAEMON_SOCKET_UNIT, NIX_DAEMON_SERVICE_UNIT]
            .iter()
            .enumerate()
        {
            let state = facts.unit(index);
            if state.active_state == "active" {
                actions.push(Action::StopUnit {
                    unit: unit.to_string(),
                });
            }
            if state.enabled() {
                actions.push(Action::DisableUnit {
                    unit: unit.to_string(),
                });
            }
        }
        let mut units_removed = false;
        for (offset, path) in UNIT_FILES.iter().enumerate() {
            if let Some(action) = set_aside(Path::new(path), facts.path(2 + offset)) {
                actions.push(action);
                units_removed = true;
            }
        }
        if units_removed {
            actions.push(Action::DaemonReload);
        }
        let users = 2 + UNIT_FILES.len();
        for n in 1..=NIXBLD_USER_COUNT {
            if let Some(user) = facts.user(users + n as usize - 1) {
                actions.push(Action::DeleteUser {
                    name: user_name(n).into_owned(),
                    expect: (user.uid, user.gid),
                    comment: user.comment.clone(),
                });
            }
        }
        let groups = users + NIXBLD_USER_COUNT as usize;
        for (offset, name) in [NIXBLD_GROUP, MIX_USERS_GROUP].iter().enumerate() {
            if let Some(group) = facts.group(groups + offset) {
                actions.push(Action::DeleteGroup {
                    name: name.to_string(),
                    expect: group.gid,
                });
            }
        }
        let paths = groups + 2;
        for (offset, path) in SET_ASIDE.iter().enumerate() {
            actions.extend(set_aside(Path::new(path), facts.path(paths + offset)));
        }
        Ok(actions)
    }
}

struct CreateNixDir;

impl StepSpec for CreateNixDir {
    fn key(&self) -> Cow<'static, str> {
        "create-nix-dir".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "create /nix".into()
    }

    fn queries(&self) -> Vec<Query> {
        vec![
            Query::Path("/nix".into()),
            Query::Path(NIX_OWNERSHIP_MARKER.into()),
            Query::Contents(NIX_OWNERSHIP_MARKER.into()),
        ]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let facts = Facts(facts);
        let mut actions = ensure_dir(Path::new("/nix"), facts.path(0), DIR_MODE, None)?;
        actions.extend(ensure_file(
            Path::new(NIX_OWNERSHIP_MARKER),
            facts.path(1),
            facts.contents(2),
            b"",
            FILE_MODE,
            None,
        )?);
        Ok(actions)
    }
}

struct CreateNixTree;

impl StepSpec for CreateNixTree {
    fn key(&self) -> Cow<'static, str> {
        "create-nix-tree".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "create managed runtime directory tree".into()
    }

    fn queries(&self) -> Vec<Query> {
        NIX_TREE_PATHS
            .iter()
            .map(|path| Query::Path(path.into()))
            .collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let facts = Facts(facts);
        let mut actions = Vec::new();
        for (index, path) in NIX_TREE_PATHS.iter().enumerate() {
            actions.extend(ensure_dir(
                Path::new(path),
                facts.path(index),
                NIX_TREE_MODE,
                None,
            )?);
        }
        Ok(actions)
    }
}

struct CreateUsersAndGroups {
    request: String,
}

impl StepSpec for CreateUsersAndGroups {
    fn key(&self) -> Cow<'static, str> {
        "create-users-and-groups".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "create the managed groups and build users".into()
    }

    fn queries(&self) -> Vec<Query> {
        let mut queries = vec![
            Query::Path(NIXBLD_HOME.into()),
            Query::Group(NIXBLD_GROUP.to_string()),
            Query::Group(MIX_USERS_GROUP.to_string()),
        ];
        queries.extend((1..=NIXBLD_USER_COUNT).map(|n| Query::User(user_name(n).into_owned())));
        queries
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let facts = Facts(facts);
        let mut actions = ensure_dir(
            Path::new(NIXBLD_HOME),
            facts.path(0),
            NIXBLD_HOME_MODE,
            None,
        )?;
        for (index, (name, gid)) in [(NIXBLD_GROUP, NIXBLD_GID), (MIX_USERS_GROUP, MIX_USERS_GID)]
            .iter()
            .enumerate()
        {
            match facts.group(1 + index) {
                None => actions.push(Action::AddGroup {
                    name: name.to_string(),
                    gid: *gid,
                }),
                Some(group) if group.gid != *gid => actions.push(Action::SetGroupGid {
                    name: name.to_string(),
                    gid: *gid,
                    expect: group.gid,
                }),
                Some(_) => {}
            }
        }
        for n in 1..=NIXBLD_USER_COUNT {
            let name = user_name(n).into_owned();
            let uid = NIXBLD_UID_BASE + n;
            match facts.user(2 + n as usize) {
                None => actions.push(Action::AddUser(UserSpec {
                    name,
                    uid,
                    gid: NIXBLD_GID,
                    home: NIXBLD_HOME.into(),
                    shell: NIXBLD_SHELL.into(),
                    comment: format!("mix build user {n} for request {}", self.request),
                    groups: vec![NIXBLD_GROUP.to_string()],
                })),
                Some(user) if (user.uid, user.gid) != (uid, NIXBLD_GID) => {
                    actions.push(Action::SetUserIds {
                        name,
                        ids: (uid, NIXBLD_GID),
                        expect: (user.uid, user.gid),
                    });
                }
                Some(_) => {}
            }
        }
        Ok(actions)
    }
}

struct FetchRuntime(Runtime);

impl StepSpec for FetchRuntime {
    fn key(&self) -> Cow<'static, str> {
        "fetch-runtime".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "fetch and activate the managed runtime".into()
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Path(DEFAULT_PROFILE_NIX_ENV.into())]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        match Facts(facts).path(0).kind {
            Kind::Missing => Ok(vec![Action::InstallRuntime {
                url: self.0.url.clone(),
                sha256: self.0.sha256,
                size: self.0.size,
            }]),
            _ => Ok(Vec::new()),
        }
    }
}

struct ConfigureNixConf(Policy);

impl ConfigureNixConf {
    fn files(&self) -> [(&'static str, &[u8]); 3] {
        [
            (POLICY_FILE, self.0.render().as_bytes()),
            (NIX_CONF_DEST, self.0.nix_conf().as_bytes()),
            (PROFILE_SNIPPET_DEST, PROFILE_SNIPPET.as_bytes()),
        ]
    }
}

impl StepSpec for ConfigureNixConf {
    fn key(&self) -> Cow<'static, str> {
        "write-nix-conf".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "write runtime configuration".into()
    }

    fn queries(&self) -> Vec<Query> {
        let mut queries = Vec::new();
        for (path, _) in self.files() {
            let path = PathBuf::from(path);
            queries.push(Query::Path(path.parent().expect("an absolute file").into()));
            queries.push(Query::Path(path.clone()));
            queries.push(Query::Contents(path));
        }
        queries
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let facts = Facts(facts);
        let mut actions = Vec::new();
        for (index, (path, wanted)) in self.files().into_iter().enumerate() {
            let path = Path::new(path);
            let parent = path.parent().expect("an absolute file");
            actions.extend(ensure_exists(parent, facts.path(3 * index), None)?);
            actions.extend(ensure_file(
                path,
                facts.path(3 * index + 1),
                facts.contents(3 * index + 2),
                wanted,
                FILE_MODE,
                None,
            )?);
        }
        Ok(actions)
    }
}

struct ConfigureDaemon;

const UNITS: [(&str, &str, &str); 2] = [
    (
        NIX_DAEMON_SERVICE_UNIT,
        NIX_DAEMON_SERVICE_SRC,
        NIX_DAEMON_SERVICE_DEST,
    ),
    (
        NIX_DAEMON_SOCKET_UNIT,
        NIX_DAEMON_SOCKET_SRC,
        NIX_DAEMON_SOCKET_DEST,
    ),
];

impl StepSpec for ConfigureDaemon {
    fn key(&self) -> Cow<'static, str> {
        "configure-daemon".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "configure the managed background service".into()
    }

    fn queries(&self) -> Vec<Query> {
        let mut queries = Vec::new();
        for (_, source, destination) in UNITS {
            queries.push(Query::Contents(source.into()));
            queries.push(Query::Path(destination.into()));
            queries.push(Query::Contents(destination.into()));
        }
        queries.push(Query::Unit(NIX_DAEMON_SOCKET_UNIT.to_string()));
        queries.push(Query::Unit(NIX_DAEMON_SERVICE_UNIT.to_string()));
        queries.push(Query::Path(NIX_CONF_DEST.into()));
        queries
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let facts = Facts(facts);
        let mut actions = Vec::new();
        for (index, (unit, source, _)) in UNITS.iter().enumerate() {
            let wanted = facts.contents(3 * index).ok_or_else(|| Failure::Io {
                path: source.into(),
                kind: std::io::ErrorKind::NotFound,
            })?;
            let installed = facts.path(3 * index + 1);
            if facts.contents(3 * index + 2) != Some(wanted) {
                actions.push(Action::InstallUnit {
                    unit: unit.to_string(),
                    contents: Arc::from(wanted),
                    expect: installed.id.map_or(Expect::Absent, Expect::Present),
                });
            }
        }
        let socket = facts.unit(6);
        if !actions.is_empty() || socket.needs_reload {
            actions.push(Action::DaemonReload);
        }
        match socket.file_state.as_str() {
            "enabled" | "static" | "indirect" | "generated" | "alias" => {}
            "masked" | "masked-runtime" => {
                return Err(Failure::Conflict {
                    subject: NIX_DAEMON_SOCKET_UNIT.to_string(),
                    expected: "a unit mix may enable".to_string(),
                    found: format!("a {} unit", socket.file_state),
                });
            }
            _ => actions.push(Action::EnableUnit {
                unit: NIX_DAEMON_SOCKET_UNIT.to_string(),
            }),
        }
        if socket.active_state != "active" {
            actions.push(Action::StartUnit {
                unit: NIX_DAEMON_SOCKET_UNIT.to_string(),
            });
        }
        let service = facts.unit(7);
        let configured = facts.path(8).changed;
        if service.active_state == "active"
            && let (Some(since), Some(configured)) = (service.active_since, configured)
            && configured > since
        {
            actions.push(Action::RestartUnit {
                unit: NIX_DAEMON_SERVICE_UNIT.to_string(),
            });
        }
        Ok(actions)
    }
}

struct WriteHomeConfig(UserConfig);

impl WriteHomeConfig {
    fn owner(&self) -> Owner {
        (self.0.user.uid, self.0.user.gid)
    }

    fn intermediate(&self) -> [PathBuf; 3] {
        let home = &self.0.user.home;
        [
            home.join(".local"),
            home.join(".local/state"),
            home.join(".local/state/nix"),
        ]
    }

    fn files(&self) -> [(PathBuf, &str); 3] {
        let state = mix_state_dir(&self.0.user.home);
        [
            (state.join(HOME_NIX), &self.0.home),
            (state.join(FLAKE_NIX), &self.0.flake),
            (state.join(FLAKE_LOCK), &self.0.lock),
        ]
    }
}

impl StepSpec for WriteHomeConfig {
    fn key(&self) -> Cow<'static, str> {
        "write-home-config".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "write home-manager config".into()
    }

    fn queries(&self) -> Vec<Query> {
        let home = &self.0.user.home;
        let mut queries = vec![Query::Path(home.clone())];
        queries.extend(self.intermediate().into_iter().map(Query::Path));
        queries.push(Query::Path(mix_state_dir(home)));
        queries.push(Query::Path(nix_profiles_dir(home)));
        for (path, _) in self.files() {
            queries.push(Query::Path(path.clone()));
            queries.push(Query::Contents(path));
        }
        queries.push(Query::Path(mix_state_dir(home).join(STATE_FILE)));
        queries.push(Query::Contents(mix_state_dir(home).join(STATE_FILE)));
        queries.push(Query::Group(MIX_USERS_GROUP.to_string()));
        queries
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let facts = Facts(facts);
        let home = &self.0.user.home;
        let owner = Some(self.owner());
        if facts.path(0).kind != Kind::Directory {
            return Err(refuse(home, "a home directory", facts.path(0).kind));
        }
        let mut actions = Vec::new();
        for (index, path) in self.intermediate().iter().enumerate() {
            actions.extend(ensure_exists(path, facts.path(1 + index), owner)?);
        }
        actions.extend(ensure_dir(
            &mix_state_dir(home),
            facts.path(4),
            MIX_STATE_DIR_MODE,
            owner,
        )?);
        actions.extend(ensure_dir(
            &nix_profiles_dir(home),
            facts.path(5),
            NIX_PROFILES_DIR_MODE,
            owner,
        )?);
        for (index, (path, wanted)) in self.files().iter().enumerate() {
            actions.extend(ensure_file(
                path,
                facts.path(6 + 2 * index),
                facts.contents(7 + 2 * index),
                wanted.as_bytes(),
                FILE_MODE,
                owner,
            )?);
        }
        let state = mix_state_dir(home).join(STATE_FILE);
        actions.extend(match &self.0.restored_state {
            Some(restored) => ensure_file(
                &state,
                facts.path(12),
                facts.contents(13),
                restored.as_bytes(),
                FILE_MODE,
                owner,
            )?,
            None => ensure_seeded(
                &state,
                facts.path(12),
                StateManifest::seed_rendered().as_bytes(),
                owner,
            )?,
        });
        let enrolled = facts
            .group(14)
            .is_some_and(|group| group.members.contains(&self.0.user.name));
        if !enrolled {
            actions.push(Action::AddMember {
                group: MIX_USERS_GROUP.to_string(),
                user: self.0.user.name.clone(),
            });
        }
        Ok(actions)
    }
}

struct ActivateHome(UserConfig);

impl StepSpec for ActivateHome {
    fn key(&self) -> Cow<'static, str> {
        "activate-home".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "activate home-manager config".into()
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Path(mix_state_dir(&self.0.user.home).join(".git"))]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        if Facts(facts).path(0).kind != Kind::Missing {
            return Ok(Vec::new());
        }
        Ok(vec![
            Action::ActivateProfile {
                user: self.0.user.clone(),
                allow_source_builds: true,
            },
            Action::RecordState {
                user: self.0.user.clone(),
            },
        ])
    }
}

pub fn steps(settings: &Settings) -> Vec<Box<dyn StepSpec>> {
    let mut steps: Vec<Box<dyn StepSpec>> = Vec::new();
    if settings.force {
        steps.push(Box::new(RemoveExistingInstallation));
    }
    steps.push(Box::new(CreateNixDir));
    steps.push(Box::new(CreateNixTree));
    steps.push(Box::new(CreateUsersAndGroups {
        request: settings.request.clone(),
    }));
    steps.push(Box::new(FetchRuntime(settings.runtime.clone())));
    steps.push(Box::new(ConfigureNixConf(settings.policy.clone())));
    steps.push(Box::new(ConfigureDaemon));
    if let Some(user) = &settings.user {
        steps.push(Box::new(WriteHomeConfig(user.clone())));
        steps.push(Box::new(ActivateHome(user.clone())));
    }
    steps
}

#[cfg(test)]
mod tests;
