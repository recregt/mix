use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use mix_core::identity::InvokingUser;
use mix_core::paths::{STATE_FILE, is_leftover, mix_state_dir, repository_dir};
use mix_core::world::{Content, World};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    Directory {
        mode: u32,
        owner: (u32, u32),
        ino: u64,
    },
    File {
        mode: u32,
        owner: (u32, u32),
        ino: u64,
        digest: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub intact: bool,
    pub recorded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub gid: u32,
    pub members: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    pub enabled: bool,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub files: BTreeMap<PathBuf, Node>,
    pub repository: Option<Repository>,
    pub journals: BTreeSet<String>,
    pub list: Option<String>,
    pub active: Option<String>,
    pub generations: Vec<u64>,
    pub groups: BTreeMap<String, Group>,
    pub users: BTreeMap<String, Account>,
    pub units: BTreeMap<String, Unit>,
}

impl Observation {
    pub fn leftovers(&self) -> Vec<&Path> {
        self.files
            .keys()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_leftover)
            })
            .map(PathBuf::as_path)
            .collect()
    }

    pub fn repository_records_the_config(&self) -> bool {
        self.repository
            == Some(Repository {
                intact: true,
                recorded: true,
            })
    }
}

pub fn digest(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

fn text(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(|bytes| String::from_utf8_lossy(bytes).into_owned())
}

pub fn of_world(world: &World, user: &InvokingUser) -> Observation {
    let files = world
        .files
        .iter()
        .map(|(path, entry)| {
            let node = match &entry.content {
                Content::Directory => Node::Directory {
                    mode: entry.mode,
                    owner: entry.owner,
                    ino: entry.id.ino,
                },
                Content::File(bytes) => Node::File {
                    mode: entry.mode,
                    owner: entry.owner,
                    ino: entry.id.ino,
                    digest: digest(bytes),
                },
            };
            (path.clone(), node)
        })
        .collect();
    let repository = world
        .files
        .contains_key(&repository_dir(&user.home))
        .then(|| Repository {
            intact: world.verifies(user),
            recorded: world
                .committed(user)
                .is_some_and(|recorded| recorded == world.staged(user)),
        });
    let profile = world.profile(user);
    Observation {
        files,
        repository,
        journals: world.logs.keys().cloned().collect(),
        list: text(world.contents(mix_state_dir(&user.home).join(STATE_FILE))),
        active: text(world.active_list(user)),
        generations: profile
            .map(|profile| profile.generations.clone())
            .unwrap_or_default(),
        groups: world
            .groups
            .iter()
            .map(|(name, group)| {
                (
                    name.clone(),
                    Group {
                        gid: group.gid,
                        members: group.members.iter().cloned().collect(),
                    },
                )
            })
            .collect(),
        users: world
            .users
            .iter()
            .map(|(name, account)| {
                (
                    name.clone(),
                    Account {
                        uid: account.uid,
                        gid: account.gid,
                        home: account.home.clone(),
                        shell: account.shell.clone(),
                    },
                )
            })
            .collect(),
        units: world
            .units
            .iter()
            .map(|(name, unit)| {
                (
                    name.clone(),
                    Unit {
                        enabled: unit.enabled,
                        running: unit.running.is_some(),
                    },
                )
            })
            .filter(|(_, unit)| unit.enabled || unit.running)
            .collect(),
    }
}
