use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Digest, Expect, FileId, Owner};
use crate::declared::identity::InvokingUser;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSpec {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
    pub comment: String,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlakeSource {
    Path,
    Git,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    CreateDir {
        path: PathBuf,
        mode: u32,
        owner: Option<Owner>,
    },
    CreateDirs {
        path: PathBuf,
        mode: u32,
        owner: Option<Owner>,
    },
    PutFile {
        path: PathBuf,
        contents: Arc<[u8]>,
        mode: u32,
        owner: Option<Owner>,
        expect: Expect,
    },
    SetMode {
        path: PathBuf,
        mode: u32,
        expect: u32,
    },
    SetOwner {
        path: PathBuf,
        owner: Owner,
        expect: Owner,
    },
    SetAside {
        path: PathBuf,
        expect: FileId,
    },
    RemoveCreated {
        path: PathBuf,
        expect: FileId,
    },
    RemoveCreatedTree {
        path: PathBuf,
        expect: FileId,
    },
    Restore {
        path: PathBuf,
        from: PathBuf,
        expect: Expect,
    },
    ReclaimTree {
        path: PathBuf,
        expect: FileId,
        owner: Owner,
        mode: u32,
    },
    CopyTree {
        from: PathBuf,
        to: PathBuf,
        owner: Owner,
        mode: u32,
    },
    AddGroup {
        name: String,
        gid: u32,
    },
    SetGroupGid {
        name: String,
        gid: u32,
        expect: u32,
    },
    DeleteGroup {
        name: String,
        expect: u32,
    },
    AddUser(UserSpec),
    SetUserIds {
        name: String,
        ids: Owner,
        expect: Owner,
    },
    DeleteUser {
        name: String,
        expect: Owner,
        comment: String,
    },
    AddMember {
        group: String,
        user: String,
    },
    RemoveMember {
        group: String,
        user: String,
    },
    InstallUnit {
        unit: String,
        contents: Arc<[u8]>,
        expect: Expect,
    },
    EnableUnit {
        unit: String,
    },
    DisableUnit {
        unit: String,
    },
    StartUnit {
        unit: String,
    },
    StopUnit {
        unit: String,
    },
    RestartUnit {
        unit: String,
    },
    DrainService {
        unit: String,
    },
    DaemonReload,
    InstallRuntime {
        url: String,
        sha256: Digest,
        size: u64,
    },
    RemoveRuntime {
        created: Vec<PathBuf>,
        kept: Vec<PathBuf>,
    },
    ActivateProfile {
        user: InvokingUser,
        source: FlakeSource,
    },
    SwitchGeneration {
        user: InvokingUser,
        generation: Option<u64>,
        expect: Option<u64>,
    },
    DeleteGeneration {
        user: InvokingUser,
        generation: u64,
    },
    ApplyGeneration {
        user: InvokingUser,
    },
    RecordState {
        user: InvokingUser,
    },
    CollectGarbage {
        user: InvokingUser,
    },
    CreateRepository {
        user: InvokingUser,
    },
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Path(PathBuf),
    Group(String),
    User(String),
    Unit(String),
    Profile(InvokingUser),
}

impl Action {
    pub fn subjects(&self) -> Vec<Subject> {
        use crate::declared::paths::{
            DEFAULT_PROFILE_NIX_ENV, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, NIX_DAEMON_SERVICE_SRC,
            NIX_DAEMON_SOCKET_SRC, NIX_STORE, STATE_FILE, SYSTEMD_UNIT_DIR, mix_state_dir,
            repository_dir,
        };
        let path = |path: &std::path::Path| Subject::Path(path.to_path_buf());
        let unit = |unit: &str| {
            vec![
                Subject::Unit(unit.to_string()),
                Subject::Path(std::path::Path::new(SYSTEMD_UNIT_DIR).join(unit)),
            ]
        };
        match self {
            Action::CreateDir { path: at, .. }
            | Action::CreateDirs { path: at, .. }
            | Action::PutFile { path: at, .. }
            | Action::SetMode { path: at, .. }
            | Action::SetOwner { path: at, .. }
            | Action::SetAside { path: at, .. }
            | Action::RemoveCreated { path: at, .. }
            | Action::RemoveCreatedTree { path: at, .. }
            | Action::ReclaimTree { path: at, .. } => vec![path(at)],
            Action::Restore { path: at, from, .. } => vec![path(at), path(from)],
            Action::CopyTree { from, to, .. } => vec![path(from), path(to)],
            Action::AddGroup { name, .. }
            | Action::SetGroupGid { name, .. }
            | Action::DeleteGroup { name, .. } => vec![Subject::Group(name.clone())],
            Action::AddUser(spec) => std::iter::once(Subject::User(spec.name.clone()))
                .chain(spec.groups.iter().cloned().map(Subject::Group))
                .collect(),
            Action::SetUserIds { name, .. } | Action::DeleteUser { name, .. } => {
                vec![Subject::User(name.clone())]
            }
            Action::AddMember { group, user } | Action::RemoveMember { group, user } => {
                vec![Subject::Group(group.clone()), Subject::User(user.clone())]
            }
            Action::InstallUnit { unit: name, .. }
            | Action::EnableUnit { unit: name }
            | Action::DisableUnit { unit: name }
            | Action::StartUnit { unit: name }
            | Action::StopUnit { unit: name }
            | Action::RestartUnit { unit: name }
            | Action::DrainService { unit: name } => unit(name),
            Action::DaemonReload | Action::Commit => Vec::new(),
            Action::InstallRuntime { .. } => [
                NIX_STORE,
                DEFAULT_PROFILE_NIX_ENV,
                NIX_DAEMON_SERVICE_SRC,
                NIX_DAEMON_SOCKET_SRC,
            ]
            .iter()
            .map(|at| path(std::path::Path::new(at)))
            .collect(),
            Action::RemoveRuntime { created, kept } => {
                created.iter().chain(kept).map(|at| path(at)).collect()
            }
            Action::ActivateProfile { user, .. } => {
                let state = mix_state_dir(&user.home);
                let mut subjects = vec![
                    Subject::Profile(user.clone()),
                    path(std::path::Path::new(DEFAULT_PROFILE_NIX_ENV)),
                ];
                subjects.extend(
                    [FLAKE_NIX, HOME_NIX, FLAKE_LOCK, STATE_FILE]
                        .iter()
                        .map(|file| path(&state.join(file))),
                );
                subjects
            }
            Action::SwitchGeneration { user, .. }
            | Action::DeleteGeneration { user, .. }
            | Action::ApplyGeneration { user }
            | Action::CollectGarbage { user } => vec![Subject::Profile(user.clone())],
            Action::RecordState { user } | Action::CreateRepository { user } => {
                let repository = repository_dir(&user.home);
                vec![
                    path(&repository.join(crate::declared::paths::REPOSITORY_HEAD)),
                    path(&repository),
                ]
            }
        }
    }

    pub fn is_sync(&self) -> bool {
        matches!(
            self,
            Action::DaemonReload | Action::RestartUnit { .. } | Action::ApplyGeneration { .. }
        )
    }

    pub fn needs_loaded_units(&self) -> bool {
        matches!(
            self,
            Action::EnableUnit { .. }
                | Action::DisableUnit { .. }
                | Action::StartUnit { .. }
                | Action::StopUnit { .. }
        )
    }
}
