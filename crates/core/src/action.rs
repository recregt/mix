use std::path::PathBuf;
use std::sync::Arc;

use crate::privilege::InvokingUser;

pub type Owner = (u32, u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
    pub born: Option<(i64, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Digest(pub [u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Absent,
    Present(FileId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSpec {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
    pub comment: String,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CreateDir {
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
    Restore {
        path: PathBuf,
        from: PathBuf,
        expect: Expect,
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
    DaemonReload,
    InstallRuntime {
        url: String,
        sha256: Digest,
        size: u64,
    },
    RemoveRuntime {
        created: Vec<PathBuf>,
    },
    ActivateProfile {
        user: InvokingUser,
        allow_source_builds: bool,
    },
    SwitchGeneration {
        user: InvokingUser,
        generation: Option<u64>,
        expect: Option<u64>,
    },
    RecordState {
        user: InvokingUser,
    },
    Commit,
}

impl Action {
    pub fn is_sync(&self) -> bool {
        matches!(self, Action::DaemonReload | Action::RestartUnit { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Performed {
    pub undo: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Conflict {
        subject: String,
        expected: String,
        found: String,
    },
    Io {
        path: PathBuf,
        kind: std::io::ErrorKind,
    },
    CommandFailed {
        program: String,
        status: Option<i32>,
        output_tail: String,
    },
    SpawnFailed {
        program: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFailure {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Missing,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFacts {
    pub load_state: String,
    pub active_state: String,
    pub enabled: bool,
    pub needs_reload: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    Path(PathFacts),
    Contents(Option<Arc<[u8]>>),
    Group(Option<GroupFacts>),
    User(Option<UserFacts>),
    Unit(UnitFacts),
}

pub fn rollback_order(journal: &[Vec<Action>]) -> Vec<Action> {
    let mut ordered = Vec::new();
    let mut syncs: Vec<Action> = Vec::new();
    for undo in journal.iter().rev() {
        for action in undo {
            if action.is_sync() {
                if !syncs.contains(action) {
                    syncs.push(action.clone());
                }
            } else {
                ordered.push(action.clone());
            }
        }
    }
    syncs.reverse();
    ordered.extend(syncs);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(name: &str) -> String {
        name.to_string()
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
    fn an_empty_journal_undoes_nothing() {
        assert!(rollback_order(&[]).is_empty());
    }
}
