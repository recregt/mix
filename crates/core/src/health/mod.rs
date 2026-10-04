use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use crate::action::{
    Action, Expect, Fact, Failure, Kind, Owner, PathFacts, ProgramFacts, Query, UserSpec,
};
use crate::bootstrap::stale_restart;
use crate::identity;
use crate::paths::{INDEX_LOCK, NIX_CONF_DEST, NIX_DAEMON_SERVICE_UNIT};
use crate::plan::{StepSpec, Title};
use crate::targets::{Target, UnitSource};
use mix_events::v1::Verb;

const FILE_MODE: u32 = 0o644;

/// What the audit found about one target. The daemon sends it to the client as an
/// `InspectionReport`.
pub struct HealthReport {
    pub name: String,
    pub category: crate::targets::Category,
    pub finding: Option<Finding>,
    pub drift: Option<Drift>,
}

impl HealthReport {
    pub fn healthy(&self) -> bool {
        self.finding.is_none()
    }
}

/// Why an artifact is beyond repair's reach.
///
/// The reason is a value, not a sentence. What a reader should do about it depends on the
/// command: `mix repair` offers a way out, and the health gate in front of the other commands
/// only says why it stopped. So the words are chosen where the command is known.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize,
)]
pub enum Unfixable {
    /// Something is in the way that repair will not delete.
    #[error("exists but is not a directory")]
    NotADirectory,

    /// The user the membership was for is gone.
    #[error("the user no longer exists")]
    MissingUser,

    /// Part of the Nix runtime itself, which repair does not install.
    #[error("missing, and `mix repair` can't restore it")]
    MissingRuntime,

    /// An interrupted request whose recovery keeps failing.
    #[error("an interrupted request couldn't be put back")]
    Unrecovered,

    /// A user's file where a file mix manages belongs, which repair will not delete.
    #[error("in the way of a file `mix` manages")]
    InTheWay,
}

/// What an inspection measured about an artifact that is not as it should be.
///
/// Every variant is a fact and nothing else: no advice, no sentence, no name. The artifact is
/// named by the report that carries the finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// Nothing is at the path.
    Missing,

    /// The path is there but could not be read.
    Unreadable { kind: std::io::ErrorKind },

    /// Something else is in the place of the directory.
    NotADirectory,

    /// The permission bits drifted from the ones mix sets.
    Mode { actual: u32, expected: u32 },

    /// The owning uid/gid drifted from the ones mix sets.
    Owner {
        actual: (u32, u32),
        expected: (u32, u32),
    },

    /// The file is mix's to write, and its contents are no longer the ones mix wrote.
    ContentDrift,

    /// There is no such group.
    GroupMissing,

    /// The group exists under a different gid.
    GroupGid { actual: u32, expected: u32 },

    /// The user exists but is not enrolled in the group.
    NotAMember { group: &'static str },

    /// The user the membership was for is gone.
    NoSuchUser,

    /// There is no such user.
    UserMissing,

    /// The user exists under different ids.
    UserIds {
        actual: (u32, u32),
        expected: (u32, u32),
    },

    /// The unit file is not installed.
    UnitMissing,

    /// The installed unit file drifted from the one mix ships.
    UnitDrift,

    /// The unit is installed but not running.
    UnitInactive,

    /// Part of the Nix runtime itself is not there.
    RuntimeMissing,

    /// The repository's `HEAD` does not resolve to a commit, or `git fsck` fails.
    RepositoryBroken,

    /// The repository's index lock is there while no mix command holds the repository.
    RepositoryLocked,

    /// Journals of requests that were interrupted and that no running request holds, and the
    /// subjects their recovery still has to put back.
    Interrupted {
        requests: Vec<String>,
        pending: Vec<String>,
    },

    /// Siblings an interrupted write left beside the files it was replacing.
    Leftovers { paths: Vec<String> },

    /// A generation of the user's profile whose link no longer resolves.
    GenerationDangling { generation: u64 },

    /// Files in the user's home where the active generation links a managed file.
    InTheWay { paths: Vec<String> },
}

impl Finding {
    /// Why `mix repair` cannot reconcile this finding, or `None` when it can.
    ///
    /// Repair's reach is a fact about repair, not a choice of words, so it is bound to the
    /// finding here. The daemon sends the answer with every report, so the client never decides
    /// for itself what the reader can be promised.
    pub fn unfixable(&self) -> Option<Unfixable> {
        match self {
            Finding::NotADirectory => Some(Unfixable::NotADirectory),
            Finding::NoSuchUser => Some(Unfixable::MissingUser),
            Finding::RuntimeMissing => Some(Unfixable::MissingRuntime),
            Finding::Interrupted { .. } => Some(Unfixable::Unrecovered),
            Finding::InTheWay { .. } => Some(Unfixable::InTheWay),
            _ => None,
        }
    }
}

pub fn queries(target: &Target<'_>) -> Vec<Query> {
    match target {
        Target::Directory { path, owner, .. } => {
            let mut queries = vec![Query::Path(path.to_path_buf())];
            if owner.is_some_and(|(uid, _)| uid != 0) {
                queries.push(Query::TreeOwner(path.to_path_buf()));
            }
            queries
        }
        Target::File { path, owner, .. } => {
            let mut queries = vec![
                Query::Path(path.to_path_buf()),
                Query::Contents(path.to_path_buf()),
            ];
            if owner.is_some_and(|(uid, _)| uid != 0) {
                queries.push(Query::TreeOwner(path.to_path_buf()));
            }
            queries
        }
        Target::SeededFile { path, owner, .. } => {
            let mut queries = vec![Query::Path(path.to_path_buf())];
            if owner.is_some_and(|(uid, _)| uid != 0) {
                queries.push(Query::TreeOwner(path.to_path_buf()));
            }
            queries
        }
        Target::Group { name, .. } => vec![Query::Group((*name).to_string())],
        Target::GroupMember { group, user } => vec![
            Query::Group((*group).to_string()),
            Query::User(user.clone()),
        ],
        Target::User { n, .. } => vec![Query::User(identity::user_name(*n).into_owned())],
        Target::SystemdUnit {
            name, src, dest, ..
        } => {
            let mut queries = vec![
                Query::Contents((*dest).into()),
                Query::Path((*dest).into()),
                Query::Unit((*name).to_string()),
            ];
            if let UnitSource::File(path) = src {
                queries.push(Query::Contents((*path).into()));
            }
            queries
        }
        Target::PathExists { path, .. } => vec![Query::Path((*path).into())],
        Target::Repository { path, user } => vec![
            Query::Path(path.join(INDEX_LOCK)),
            Query::Path(path.to_path_buf()),
            Query::Repository(user.clone().into_owned()),
            Query::Strangers {
                path: path.to_path_buf(),
                owner: (user.uid, user.gid),
            },
            Query::TreeOwner(path.to_path_buf()),
        ],
        Target::Program { path, source, .. } => vec![
            Query::Path((*path).into()),
            Query::Program {
                path: (*path).into(),
                source: (*source).into(),
            },
        ],
        Target::Journals { path } => vec![Query::Journals((*path).into())],
        Target::Leftovers { user, journals } => {
            let mut queries = vec![Query::Journals((*journals).into())];
            queries.extend(
                crate::targets::written_dirs(user.as_deref())
                    .into_iter()
                    .map(Query::Leftovers),
            );
            queries
        }
        Target::Generations { user, .. } => vec![Query::Profile(user.clone().into_owned())],
        Target::HomeFiles { user, .. } => vec![Query::Clobbered(user.clone().into_owned())],
    }
}

fn path_facts(facts: &[Fact], index: usize) -> &PathFacts {
    match &facts[index] {
        Fact::Path(facts) => facts,
        other => panic!("fact {index} is not a path: {other:?}"),
    }
}

fn contents(facts: &[Fact], index: usize) -> Option<&[u8]> {
    match &facts[index] {
        Fact::Contents(contents) => contents.as_deref(),
        other => panic!("fact {index} is not contents: {other:?}"),
    }
}

fn unit_source<'f>(src: &UnitSource, facts: &'f [Fact]) -> Option<&'f [u8]> {
    match src {
        UnitSource::File(_) => contents(facts, 3),
        UnitSource::Text(text) => Some(text.as_bytes()),
    }
}

fn absent(kind: Kind) -> Option<Finding> {
    match kind {
        Kind::Missing => Some(Finding::Missing),
        Kind::Unreadable(kind) => Some(Finding::Unreadable { kind }),
        _ => None,
    }
}

fn owner_drift(actual: (u32, u32), owner: Option<(u32, u32)>) -> Option<Finding> {
    let expected = owner?;
    (actual != expected).then_some(Finding::Owner { actual, expected })
}

const SECRET_SETTINGS: &[&str] = &["access-tokens"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub path: String,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub found_line: u32,
    pub found: Vec<String>,
    pub expected: Vec<String>,
}

fn hidden(line: &str) -> String {
    match line.split_once('=') {
        Some((key, _)) if SECRET_SETTINGS.contains(&key.trim().trim_start_matches("extra-")) => {
            format!("{} = <hidden>", key.trim())
        }
        _ => line.to_string(),
    }
}

fn hunks(expected: &str, found: &str) -> Vec<Hunk> {
    let expected: Vec<&str> = expected.lines().collect();
    let found: Vec<&str> = found.lines().collect();
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut open = false;
    for op in similar::capture_diff_slices(similar::Algorithm::Myers, &expected, &found) {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == similar::DiffTag::Equal {
            open = false;
            continue;
        }
        if !open {
            hunks.push(Hunk {
                found_line: u32::try_from(new.start + 1).unwrap_or(u32::MAX),
                found: Vec::new(),
                expected: Vec::new(),
            });
            open = true;
        }
        if let Some(hunk) = hunks.last_mut() {
            hunk.found
                .extend(found[new].iter().map(|line| hidden(line)));
            hunk.expected
                .extend(expected[old].iter().map(|line| hidden(line)));
        }
    }
    hunks
}

fn text(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

pub fn drift(target: &Target<'_>, facts: &[Fact]) -> Option<Drift> {
    match target {
        Target::File {
            path,
            expected: Some(expected),
            ..
        } => {
            let current = contents(facts, 1)?;
            (current != expected.as_bytes()).then(|| Drift {
                path: path.display().to_string(),
                hunks: hunks(expected, &text(current)),
            })
        }
        Target::SystemdUnit { src, dest, .. } => {
            let installed = contents(facts, 0)?;
            let source = unit_source(src, facts)?;
            (installed != source).then(|| Drift {
                path: (*dest).to_string(),
                hunks: hunks(&text(source), &text(installed)),
            })
        }
        Target::Directory { .. }
        | Target::File { expected: None, .. }
        | Target::SeededFile { .. }
        | Target::Group { .. }
        | Target::GroupMember { .. }
        | Target::User { .. }
        | Target::PathExists { .. }
        | Target::Repository { .. }
        | Target::Program { .. }
        | Target::Journals { .. }
        | Target::Leftovers { .. }
        | Target::Generations { .. }
        | Target::HomeFiles { .. } => None,
    }
}

pub fn classify(target: &Target<'_>, facts: &[Fact]) -> Option<Finding> {
    match target {
        Target::Directory { mode, owner, .. } => {
            let found = path_facts(facts, 0);
            if let Some(finding) = absent(found.kind) {
                return Some(finding);
            }
            if found.kind != Kind::Directory {
                return Some(Finding::NotADirectory);
            }
            if found.mode != *mode {
                return Some(Finding::Mode {
                    actual: found.mode,
                    expected: *mode,
                });
            }
            owner_drift(found.owner, *owner)
        }
        Target::File {
            expected, owner, ..
        } => {
            let found = path_facts(facts, 0);
            match expected {
                Some(expected) => {
                    if let Some(finding) = absent(found.kind) {
                        return Some(finding);
                    }
                    match contents(facts, 1) {
                        Some(current) if current == expected.as_bytes() => {
                            owner_drift(found.owner, *owner)
                        }
                        Some(_) => Some(Finding::ContentDrift),
                        None => Some(Finding::Unreadable {
                            kind: if found.kind == Kind::Directory {
                                std::io::ErrorKind::IsADirectory
                            } else {
                                std::io::ErrorKind::InvalidData
                            },
                        }),
                    }
                }
                None if owner.is_none() || found.kind == Kind::Missing => None,
                None => owner_drift(found.owner, *owner),
            }
        }
        Target::SeededFile { owner, .. } => {
            let found = path_facts(facts, 0);
            absent(found.kind).or_else(|| owner_drift(found.owner, *owner))
        }
        Target::Group { gid, .. } => match &facts[0] {
            Fact::Group(None) => Some(Finding::GroupMissing),
            Fact::Group(Some(group)) if group.gid != *gid => Some(Finding::GroupGid {
                actual: group.gid,
                expected: *gid,
            }),
            _ => None,
        },
        Target::GroupMember { group, user } => {
            let member =
                matches!(&facts[0], Fact::Group(Some(found)) if found.members.contains(user));
            match (member, &facts[1]) {
                (true, _) => None,
                (false, Fact::User(Some(_))) => Some(Finding::NotAMember { group }),
                (false, _) => Some(Finding::NoSuchUser),
            }
        }
        Target::User { uid, gid, .. } => match &facts[0] {
            Fact::User(None) => Some(Finding::UserMissing),
            Fact::User(Some(found)) if (found.uid, found.gid) != (*uid, *gid) => {
                Some(Finding::UserIds {
                    actual: (found.uid, found.gid),
                    expected: (*uid, *gid),
                })
            }
            _ => None,
        },
        Target::SystemdUnit {
            src,
            must_be_active,
            ..
        } => {
            let installed = contents(facts, 0);
            let Some(installed) = installed else {
                return match path_facts(facts, 1).kind {
                    Kind::Missing => Some(Finding::UnitMissing),
                    Kind::Unreadable(kind) => Some(Finding::Unreadable { kind }),
                    _ => Some(Finding::UnitDrift),
                };
            };
            if unit_source(src, facts).is_some_and(|source| source != installed) {
                return Some(Finding::UnitDrift);
            }
            let active = matches!(
                &facts[2],
                Fact::Unit(unit) if unit.active_state == "active" || unit.active_state == "reloading"
            );
            (*must_be_active && !active).then_some(Finding::UnitInactive)
        }
        Target::PathExists { .. } => {
            (path_facts(facts, 0).kind == Kind::Missing).then_some(Finding::RuntimeMissing)
        }
        Target::Repository { user, .. } => {
            let repository = path_facts(facts, 1);
            if let Some(finding) = absent(repository.kind) {
                return Some(finding);
            }
            if repository.kind != Kind::Directory {
                return Some(Finding::RepositoryBroken);
            }
            if let Fact::Stranger(Some((_, actual))) = &facts[3] {
                return Some(Finding::Owner {
                    actual: *actual,
                    expected: (user.uid, user.gid),
                });
            }
            if !matches!(facts[2], Fact::Repository { intact: true, .. }) {
                return Some(Finding::RepositoryBroken);
            }
            (path_facts(facts, 0).kind != Kind::Missing).then_some(Finding::RepositoryLocked)
        }
        Target::Program { mode, .. } => {
            let found = path_facts(facts, 0);
            if let Some(finding) = absent(found.kind) {
                return Some(finding);
            }
            if matches!(&facts[1], Fact::Program(program) if !program.same) {
                return Some(Finding::ContentDrift);
            }
            if found.mode != *mode {
                return Some(Finding::Mode {
                    actual: found.mode,
                    expected: *mode,
                });
            }
            owner_drift(found.owner, Some((0, 0)))
        }
        Target::Journals { .. } => match &facts[0] {
            Fact::Journals(abandoned) if !abandoned.is_empty() => {
                let mut pending: Vec<String> = abandoned
                    .iter()
                    .flat_map(|request| request.pending.iter().cloned())
                    .collect();
                pending.sort();
                pending.dedup();
                Some(Finding::Interrupted {
                    requests: abandoned
                        .iter()
                        .map(|request| request.request.clone())
                        .collect(),
                    pending,
                })
            }
            _ => None,
        },
        Target::Leftovers { .. } => {
            let paths: Vec<String> = leftovers(facts)
                .map(|(path, _)| path.display().to_string())
                .collect();
            (!paths.is_empty()).then_some(Finding::Leftovers { paths })
        }
        Target::Generations { .. } => {
            let Fact::Profile(profile) = &facts[0] else {
                return None;
            };
            let generation = profile
                .active
                .filter(|active| profile.dangling.contains(active))
                .or_else(|| profile.dangling.first().copied())?;
            Some(Finding::GenerationDangling { generation })
        }
        Target::HomeFiles { .. } => match &facts[0] {
            Fact::Clobbered(paths) if !paths.is_empty() => Some(Finding::InTheWay {
                paths: paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            }),
            _ => None,
        },
    }
}

fn leftovers(facts: &[Fact]) -> impl Iterator<Item = &(std::path::PathBuf, crate::action::FileId)> {
    let abandoned: &[crate::action::Abandoned] = match &facts[0] {
        Fact::Journals(abandoned) => abandoned,
        _ => &[],
    };
    facts[1..]
        .iter()
        .flat_map(|fact| match fact {
            Fact::Leftovers(found) => found.as_slice(),
            _ => &[],
        })
        .filter(move |(path, _)| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            !abandoned.iter().any(|request| {
                name.contains(&format!("-{}-", request.request))
                    || name.ends_with(&format!("-{}", request.request))
            })
        })
}

fn tree_owner(facts: &[Fact], index: usize) -> Option<u32> {
    match facts.get(index) {
        Some(Fact::TreeOwner(owner)) => *owner,
        _ => None,
    }
}

fn own_again(
    path: &Path,
    found: &PathFacts,
    owner: Option<Owner>,
    tree: Option<u32>,
) -> Vec<Action> {
    let Some(owner) = owner else {
        return Vec::new();
    };
    if found.owner == owner {
        return Vec::new();
    }
    match found.id {
        Some(expect) if owner.0 != 0 && tree == Some(owner.0) => vec![Action::ReclaimTree {
            path: path.to_path_buf(),
            expect,
            owner,
            mode: found.mode,
        }],
        _ => vec![Action::SetOwner {
            path: path.to_path_buf(),
            owner,
            expect: found.owner,
        }],
    }
}

pub fn fix(
    target: &Target<'_>,
    finding: Finding,
    facts: &[Fact],
    request: &str,
) -> Result<Vec<Action>, Unfixable> {
    if let Some(reason) = finding.unfixable() {
        return Err(reason);
    }
    Ok(match target {
        Target::Directory { path, mode, owner } => {
            let found = path_facts(facts, 0);
            match found.kind {
                Kind::Missing | Kind::Unreadable(_) => vec![Action::CreateDirs {
                    path: path.to_path_buf(),
                    mode: *mode,
                    owner: *owner,
                }],
                _ => {
                    let mut actions = own_again(path, found, *owner, tree_owner(facts, 1));
                    match actions.first_mut() {
                        Some(Action::ReclaimTree {
                            mode: reclaimed, ..
                        }) => *reclaimed = *mode,
                        _ if found.mode != *mode => actions.insert(
                            0,
                            Action::SetMode {
                                path: path.to_path_buf(),
                                mode: *mode,
                                expect: found.mode,
                            },
                        ),
                        _ => {}
                    }
                    actions
                }
            }
        }
        Target::File {
            path,
            expected,
            owner,
        } => {
            let found = path_facts(facts, 0);
            let tree = tree_owner(facts, 2);
            match expected {
                Some(expected) if contents(facts, 1) != Some(expected.as_bytes()) => {
                    vec![Action::PutFile {
                        path: path.to_path_buf(),
                        contents: Arc::from(expected.as_bytes()),
                        mode: if found.kind == Kind::File {
                            found.mode
                        } else {
                            FILE_MODE
                        },
                        owner: *owner,
                        expect: found.id.map_or(Expect::Absent, Expect::Present),
                    }]
                }
                _ => own_again(path, found, *owner, tree),
            }
        }
        Target::SeededFile { path, seed, owner } => {
            let found = path_facts(facts, 0);
            match found.kind {
                Kind::Missing | Kind::Unreadable(_) => vec![Action::PutFile {
                    path: path.to_path_buf(),
                    contents: Arc::from(seed.as_bytes()),
                    mode: FILE_MODE,
                    owner: *owner,
                    expect: found.id.map_or(Expect::Absent, Expect::Present),
                }],
                _ => own_again(path, found, *owner, tree_owner(facts, 1)),
            }
        }
        Target::Group { name, gid } => match finding {
            Finding::GroupGid { actual, .. } => vec![Action::SetGroupGid {
                name: (*name).to_string(),
                gid: *gid,
                expect: actual,
            }],
            _ => vec![Action::AddGroup {
                name: (*name).to_string(),
                gid: *gid,
            }],
        },
        Target::GroupMember { group, user } => vec![Action::AddMember {
            group: (*group).to_string(),
            user: user.clone(),
        }],
        Target::User { n, uid, gid } => match finding {
            Finding::UserIds { actual, .. } => vec![Action::SetUserIds {
                name: identity::user_name(*n).into_owned(),
                ids: (*uid, *gid),
                expect: actual,
            }],
            _ => vec![Action::AddUser(UserSpec {
                name: identity::user_name(*n).into_owned(),
                uid: *uid,
                gid: *gid,
                home: identity::NIXBLD_HOME.into(),
                shell: identity::NIXBLD_SHELL.into(),
                comment: format!("mix build user {n} for request {request}"),
                groups: vec![identity::NIXBLD_GROUP.to_string()],
            })],
        },
        Target::SystemdUnit {
            name,
            src,
            must_be_active,
            ..
        } => {
            let mut actions = Vec::new();
            if finding != Finding::UnitInactive {
                let wanted = unit_source(src, facts).ok_or(Unfixable::MissingRuntime)?;
                actions.push(Action::InstallUnit {
                    unit: (*name).to_string(),
                    contents: Arc::from(wanted),
                    expect: path_facts(facts, 1)
                        .id
                        .map_or(Expect::Absent, Expect::Present),
                });
                actions.push(Action::DaemonReload);
            }
            if *must_be_active || finding == Finding::UnitInactive {
                actions.push(Action::EnableUnit {
                    unit: (*name).to_string(),
                });
                actions.push(Action::StartUnit {
                    unit: (*name).to_string(),
                });
            }
            actions
        }
        Target::PathExists { .. } => return Err(Unfixable::MissingRuntime),
        Target::Repository { path, user } => {
            let aside =
                |path, found: &PathFacts| found.id.map(|expect| Action::SetAside { path, expect });
            if let Finding::Owner { expected, .. } = finding {
                let found = path_facts(facts, 1);
                return Ok(found
                    .id
                    .map(|expect| Action::ReclaimTree {
                        path: path.to_path_buf(),
                        expect,
                        owner: expected,
                        mode: found.mode,
                    })
                    .into_iter()
                    .collect());
            }
            if finding == Finding::RepositoryLocked {
                return Ok(aside(path.join(INDEX_LOCK), path_facts(facts, 0))
                    .into_iter()
                    .collect());
            }
            let mut actions: Vec<Action> = aside(path.to_path_buf(), path_facts(facts, 1))
                .into_iter()
                .collect();
            actions.push(Action::CreateRepository {
                user: user.clone().into_owned(),
            });
            actions
        }
        Target::Program { path, mode, .. } => {
            let found = path_facts(facts, 0);
            match finding {
                Finding::Mode { actual, .. } => vec![Action::SetMode {
                    path: (*path).into(),
                    mode: *mode,
                    expect: actual,
                }],
                Finding::Owner { actual, expected } => vec![Action::SetOwner {
                    path: (*path).into(),
                    owner: expected,
                    expect: actual,
                }],
                _ => vec![Action::PutFile {
                    path: (*path).into(),
                    contents: match &facts[1] {
                        Fact::Program(ProgramFacts {
                            source: Some(source),
                            ..
                        }) => Arc::clone(source),
                        _ => return Err(Unfixable::MissingRuntime),
                    },
                    mode: *mode,
                    owner: None,
                    expect: found.id.map_or(Expect::Absent, Expect::Present),
                }],
            }
        }
        Target::Journals { .. } => return Err(Unfixable::Unrecovered),
        Target::HomeFiles { .. } => return Err(Unfixable::InTheWay),
        Target::Leftovers { .. } => leftovers(facts)
            .map(|(path, expect)| Action::RemoveCreatedTree {
                path: path.clone(),
                expect: *expect,
            })
            .collect(),
        Target::Generations { user, .. } => {
            let Fact::Profile(profile) = &facts[0] else {
                return Ok(Vec::new());
            };
            let mut actions = Vec::new();
            if profile
                .active
                .is_some_and(|active| profile.dangling.contains(&active))
            {
                actions.push(Action::ActivateProfile {
                    user: user.clone().into_owned(),
                    source: crate::action::FlakeSource::Git,
                });
            }
            actions.extend(
                profile
                    .dangling
                    .iter()
                    .map(|generation| Action::DeleteGeneration {
                        user: user.clone().into_owned(),
                        generation: *generation,
                    }),
            );
            actions
        }
    })
}

pub struct TargetStep {
    target: Target<'static>,
    label: String,
    request: String,
}

impl StepSpec for TargetStep {
    fn key(&self) -> Cow<'static, str> {
        Cow::Owned(self.label.clone())
    }

    fn title(&self) -> Title {
        Title::new(Verb::Repairing, self.label.clone())
    }

    fn queries(&self) -> Vec<Query> {
        queries(&self.target)
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let Some(finding) = classify(&self.target, facts) else {
            return Ok(Vec::new());
        };
        fix(&self.target, finding, facts, &self.request).map_err(|reason| Failure::Unrepairable {
            artifact: self.label.clone(),
            reason,
        })
    }
}

pub const RESTART_NIX_DAEMON: &str = "restart-nix-daemon";
pub const RECORD_REPAIRED: &str = "record-repaired-config";

struct RestartIfStale;

impl StepSpec for RestartIfStale {
    fn key(&self) -> Cow<'static, str> {
        RESTART_NIX_DAEMON.into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Restarting, "Nix daemon")
    }

    fn queries(&self) -> Vec<Query> {
        vec![
            Query::Unit(NIX_DAEMON_SERVICE_UNIT.to_string()),
            Query::Path(NIX_CONF_DEST.into()),
        ]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let Fact::Unit(service) = &facts[0] else {
            return Ok(Vec::new());
        };
        Ok(stale_restart(service, path_facts(facts, 1).changed)
            .into_iter()
            .collect())
    }
}

struct RecordRepaired(identity::InvokingUser);

impl StepSpec for RecordRepaired {
    fn key(&self) -> Cow<'static, str> {
        RECORD_REPAIRED.into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Recording, "repaired configuration")
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Repository(self.0.clone())]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok(match facts[0] {
            Fact::Repository { recorded: true, .. } => Vec::new(),
            _ => vec![Action::RecordState {
                user: self.0.clone(),
            }],
        })
    }
}

pub fn repair_steps(targets: Vec<Target<'_>>, request: &str) -> Vec<Box<dyn StepSpec>> {
    let user = targets.iter().find_map(|target| match target {
        Target::Repository { user, .. } => Some(user.clone().into_owned()),
        _ => None,
    });
    let mut steps = target_steps(targets, request);
    steps.push(Box::new(RestartIfStale));
    if let Some(user) = user {
        steps.push(Box::new(RecordRepaired(user)));
    }
    steps
}

pub fn target_steps(targets: Vec<Target<'_>>, request: &str) -> Vec<Box<dyn StepSpec>> {
    targets
        .into_iter()
        .map(|target| {
            let target = target.into_owned();
            Box::new(TargetStep {
                label: target.label().into_owned(),
                target,
                request: request.to_string(),
            }) as Box<dyn StepSpec>
        })
        .collect()
}

pub mod wire {
    use mix_events::v1::finding::Kind;
    use mix_events::v1::{
        Category as WireCategory, Finding as WireFinding, Generation, Gid, Ids, InspectionReport,
        Interrupted as WireInterrupted, Mode, NotAMember, Paths, Unfixable as WireUnfixable,
        Unreadable,
    };

    use super::{Drift, Finding, HealthReport, Unfixable};
    use crate::targets::Category;

    pub fn report(report: &HealthReport) -> InspectionReport {
        InspectionReport {
            target: report.name.clone(),
            category: category(report.category) as i32,
            finding: report.finding.clone().map(finding),
            drift: report.drift.as_ref().map(drift),
            unfixable: report
                .finding
                .as_ref()
                .and_then(Finding::unfixable)
                .map_or(WireUnfixable::Unspecified, unfixable) as i32,
        }
    }

    pub fn unfixable(reason: Unfixable) -> WireUnfixable {
        match reason {
            Unfixable::NotADirectory => WireUnfixable::NotADirectory,
            Unfixable::MissingUser => WireUnfixable::MissingUser,
            Unfixable::MissingRuntime => WireUnfixable::MissingRuntime,
            Unfixable::Unrecovered => WireUnfixable::Unrecovered,
            Unfixable::InTheWay => WireUnfixable::InTheWay,
        }
    }

    pub fn drift(drift: &Drift) -> mix_events::v1::Drift {
        mix_events::v1::Drift {
            path: drift.path.clone(),
            hunks: drift
                .hunks
                .iter()
                .map(|hunk| mix_events::v1::Hunk {
                    found_line: hunk.found_line,
                    found: hunk.found.clone(),
                    expected: hunk.expected.clone(),
                })
                .collect(),
        }
    }

    pub fn category(category: Category) -> WireCategory {
        match category {
            Category::Filesystem => WireCategory::Filesystem,
            Category::Identity => WireCategory::Identity,
            Category::Services => WireCategory::Services,
            Category::Configuration => WireCategory::Configuration,
        }
    }

    fn ids((actual_uid, actual_gid): (u32, u32), (expected_uid, expected_gid): (u32, u32)) -> Ids {
        Ids {
            actual_uid,
            actual_gid,
            expected_uid,
            expected_gid,
        }
    }

    pub fn finding(finding: Finding) -> WireFinding {
        let kind = match finding {
            Finding::Missing => Kind::Missing(Default::default()),
            Finding::Unreadable { kind } => Kind::Unreadable(Unreadable {
                kind: mix_events::io_kind::name(kind),
            }),
            Finding::NotADirectory => Kind::NotADirectory(Default::default()),
            Finding::Mode { actual, expected } => Kind::Mode(Mode { actual, expected }),
            Finding::Owner { actual, expected } => Kind::Owner(ids(actual, expected)),
            Finding::ContentDrift => Kind::ContentDrift(Default::default()),
            Finding::GroupMissing => Kind::GroupMissing(Default::default()),
            Finding::GroupGid { actual, expected } => Kind::GroupGid(Gid { actual, expected }),
            Finding::NotAMember { group } => Kind::NotAMember(NotAMember {
                group: group.to_string(),
            }),
            Finding::NoSuchUser => Kind::NoSuchUser(Default::default()),
            Finding::UserMissing => Kind::UserMissing(Default::default()),
            Finding::UserIds { actual, expected } => Kind::UserIds(ids(actual, expected)),
            Finding::UnitMissing => Kind::UnitMissing(Default::default()),
            Finding::UnitDrift => Kind::UnitDrift(Default::default()),
            Finding::UnitInactive => Kind::UnitInactive(Default::default()),
            Finding::RuntimeMissing => Kind::RuntimeMissing(Default::default()),
            Finding::RepositoryBroken => Kind::RepositoryBroken(Default::default()),
            Finding::RepositoryLocked => Kind::RepositoryLocked(Default::default()),
            Finding::Interrupted { requests, pending } => {
                Kind::Interrupted(WireInterrupted { requests, pending })
            }
            Finding::Leftovers { paths } => Kind::Leftovers(Paths { paths }),
            Finding::GenerationDangling { generation } => {
                Kind::GenerationDangling(Generation { generation })
            }
            Finding::InTheWay { paths } => Kind::InTheWay(Paths { paths }),
        };
        WireFinding { kind: Some(kind) }
    }
}

#[cfg(test)]
mod tests;
