use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::identity::InvokingUser;

pub type Owner = (u32, u32);

pub(crate) mod error_kind {
    use std::io::ErrorKind;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(kind: &ErrorKind, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&mix_events::io_kind::name(*kind))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ErrorKind, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(mix_events::io_kind::named(&name))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
    pub born: Option<(i64, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Digest(pub [u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expect {
    Absent,
    Present(FileId),
}

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

impl Action {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Performed {
    pub undo: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Failure {
    Conflict {
        subject: String,
        expected: String,
        found: String,
    },
    Io {
        path: PathBuf,
        #[serde(with = "error_kind")]
        kind: std::io::ErrorKind,
    },
    CommandFailed {
        program: String,
        status: Option<i32>,
        output_tail: String,
    },
    SpawnFailed {
        program: String,
        #[serde(with = "error_kind")]
        kind: std::io::ErrorKind,
    },
    Unit(Box<UnitFailure>),
    SystemdUnreachable,
    Network {
        url: String,
    },
    Integrity {
        artifact: String,
        expected: String,
        found: String,
    },
    Cancelled,
    Unrepairable {
        artifact: String,
        reason: crate::health::Unfixable,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitOperation {
    Inspect,
    Reload,
    Enable,
    Disable,
    Start,
    Stop,
    Restart,
}

impl UnitOperation {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Reload => "reload",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitFailure {
    pub operation: UnitOperation,
    pub unit: String,
    pub job_result: String,
    pub active_state: String,
    pub sub_state: String,
    pub unit_result: String,
    pub invocation: Option<String>,
}

pub type Outcome = Result<Performed, Failure>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    Path(PathBuf),
    Contents(PathBuf),
    Group(String),
    User(String),
    Unit(String),
    Profile(InvokingUser),
    TreeOwner(PathBuf),
    Repository(InvokingUser),
    Journals(PathBuf),
    Leftovers(PathBuf),
    Strangers { path: PathBuf, owner: Owner },
    Clobbered(InvokingUser),
    Program { path: PathBuf, source: PathBuf },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileFacts {
    pub generations: Vec<u64>,
    pub active: Option<u64>,
    pub dangling: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Missing,
    Unreadable(std::io::ErrorKind),
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFacts {
    pub kind: Kind,
    pub mode: u32,
    pub owner: Owner,
    pub id: Option<FileId>,
    pub digest: Option<Digest>,
    pub changed: Option<(i64, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupFacts {
    pub gid: u32,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFacts {
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFacts {
    pub load_state: String,
    pub active_state: String,
    pub file_state: String,
    pub needs_reload: bool,
    pub active_since: Option<(i64, u32)>,
}

impl UnitFacts {
    pub fn enabled(&self) -> bool {
        self.file_state == "enabled"
    }
}

/// A request that was interrupted and that no running request holds, with the subjects its
/// recovery still has to put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abandoned {
    pub request: String,
    pub pending: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    Path(PathFacts),
    Contents(Option<Arc<[u8]>>),
    Group(Option<GroupFacts>),
    User(Option<UserFacts>),
    Unit(UnitFacts),
    Profile(ProfileFacts),
    TreeOwner(Option<u32>),
    Repository { intact: bool },
    Journals(Vec<Abandoned>),
    Leftovers(Vec<(PathBuf, FileId)>),
    Stranger(Option<(PathBuf, Owner)>),
    Clobbered(Vec<PathBuf>),
    Program(ProgramFacts),
}

/// Whether an installed program is the one at its source, and the source's contents when it
/// is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramFacts {
    pub same: bool,
    pub source: Option<Arc<[u8]>>,
}

pub fn rollback_order(journal: &[Vec<Action>]) -> Vec<Action> {
    let mut ordered = Vec::new();
    let mut pending: Vec<Action> = Vec::new();
    for undo in journal.iter().rev() {
        for action in undo {
            if action.is_sync() {
                if !pending.contains(action) {
                    pending.push(action.clone());
                }
                continue;
            }
            if action.needs_loaded_units()
                && let Some(index) = pending
                    .iter()
                    .position(|sync| *sync == Action::DaemonReload)
            {
                ordered.push(pending.remove(index));
            }
            ordered.push(action.clone());
        }
    }
    pending.reverse();
    ordered.extend(pending);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(name: &str) -> String {
        name.to_string()
    }

    #[test]
    fn every_io_error_kind_survives_serialization() {
        for kind in mix_events::io_kind::KINDS {
            let failure = Failure::Io {
                path: "/home/alice".into(),
                kind: *kind,
            };
            let json = serde_json::to_string(&failure).unwrap();

            assert_eq!(serde_json::from_str::<Failure>(&json).unwrap(), failure);
        }
    }

    fn restore(path: &str) -> Action {
        Action::Restore {
            path: PathBuf::from(path),
            from: PathBuf::from(format!("{path}.mix-backup")),
            expect: Expect::Present(FileId {
                dev: 1,
                ino: 2,
                born: None,
            }),
        }
    }

    #[test]
    fn only_reloads_and_restarts_are_syncs() {
        assert!(Action::DaemonReload.is_sync());
        assert!(
            Action::RestartUnit {
                unit: unit("nix-daemon.service")
            }
            .is_sync()
        );
        assert!(
            !Action::StartUnit {
                unit: unit("nix-daemon.socket")
            }
            .is_sync()
        );
        assert!(!restore("/etc/nix/nix.conf").is_sync());
    }

    #[test]
    fn a_rollback_undoes_in_reverse_and_syncs_once_after_everything_else() {
        let journal = vec![
            vec![
                restore("/etc/systemd/system/nix-daemon.service"),
                Action::DaemonReload,
            ],
            vec![
                restore("/etc/systemd/system/nix-daemon.socket"),
                Action::DaemonReload,
            ],
            vec![Action::DaemonReload],
            vec![Action::DisableUnit {
                unit: unit("nix-daemon.socket"),
            }],
        ];

        assert_eq!(
            rollback_order(&journal),
            [
                Action::DisableUnit {
                    unit: unit("nix-daemon.socket")
                },
                restore("/etc/systemd/system/nix-daemon.socket"),
                restore("/etc/systemd/system/nix-daemon.service"),
                Action::DaemonReload,
            ]
        );
    }

    #[test]
    fn different_syncs_keep_the_order_they_were_first_needed_in() {
        let restart = Action::RestartUnit {
            unit: unit("nix-daemon.service"),
        };
        let journal = vec![
            vec![restore("/etc/nix/nix.conf"), restart.clone()],
            vec![
                restore("/etc/systemd/system/nix-daemon.service"),
                Action::DaemonReload,
            ],
        ];

        let order = rollback_order(&journal);

        assert_eq!(&order[order.len() - 2..], [restart, Action::DaemonReload]);
    }

    #[test]
    fn a_pending_reload_runs_before_a_unit_is_touched_again() {
        let socket = || unit("nix-daemon.socket");
        let journal = vec![
            vec![Action::StartUnit { unit: socket() }],
            vec![Action::EnableUnit { unit: socket() }],
            vec![restore("/etc/systemd/system/nix-daemon.socket")],
            vec![Action::DaemonReload],
            vec![restore("/nix")],
        ];

        assert_eq!(
            rollback_order(&journal),
            [
                restore("/nix"),
                restore("/etc/systemd/system/nix-daemon.socket"),
                Action::DaemonReload,
                Action::EnableUnit { unit: socket() },
                Action::StartUnit { unit: socket() },
            ]
        );
    }

    #[test]
    fn an_empty_journal_undoes_nothing() {
        assert!(rollback_order(&[]).is_empty());
    }
}
