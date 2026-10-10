use std::path::PathBuf;
use std::sync::Arc;

use super::{Digest, FileId, Owner};
use crate::declared::identity::InvokingUser;

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
    ActiveList(InvokingUser),
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
    Repository { intact: bool, recorded: bool },
    Journals(Vec<Abandoned>),
    Leftovers(Vec<(PathBuf, FileId)>),
    Stranger(Option<(PathBuf, Owner)>),
    Clobbered(Vec<PathBuf>),
    Program(ProgramFacts),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramFacts {
    pub same: bool,
    pub source: Option<Arc<[u8]>>,
}
