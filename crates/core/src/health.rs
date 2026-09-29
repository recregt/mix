use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use crate::action::{Action, Expect, Fact, Failure, Kind, Owner, PathFacts, Query, UserSpec};
use crate::bootstrap::stale_restart;
use crate::identity;
use crate::models::Target;
use crate::paths::{NIX_CONF_DEST, NIX_DAEMON_SERVICE_UNIT};
use crate::plan::StepSpec;

const FILE_MODE: u32 = 0o644;

/// Why an artifact is beyond repair's reach.
///
/// The reason is a value rather than a sentence: what a reader should do about it differs per
/// command — `mix repair` offers a way out, the health gate in front of the other commands only
/// says why it stopped — so the words are chosen where the command is known.
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
}

/// What an inspection measured about an artifact that is not as it should be.
///
/// Every variant is a fact and nothing else: no advice, no sentence, no name — the artifact is
/// named by the report that carries the finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl Finding {
    /// Why `mix repair` cannot reconcile this finding, or `None` when it can.
    ///
    /// Repair's reach is a fact about repair rather than a choice of words, so it is bound to
    /// the finding here — and both commands then read the same answer instead of each deciding
    /// for itself what the reader can be promised.
    pub fn unfixable(self) -> Option<Unfixable> {
        match self {
            Finding::NotADirectory => Some(Unfixable::NotADirectory),
            Finding::NoSuchUser => Some(Unfixable::MissingUser),
            Finding::RuntimeMissing => Some(Unfixable::MissingRuntime),
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
        } => vec![
            Query::Contents((*dest).into()),
            Query::Contents((*src).into()),
            Query::Path((*dest).into()),
            Query::Unit((*name).to_string()),
        ],
        Target::PathExists { path, .. } => vec![Query::Path((*path).into())],
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
        Target::SystemdUnit { must_be_active, .. } => {
            let installed = contents(facts, 0);
            let Some(installed) = installed else {
                return match path_facts(facts, 2).kind {
                    Kind::Missing => Some(Finding::UnitMissing),
                    Kind::Unreadable(kind) => Some(Finding::Unreadable { kind }),
                    _ => Some(Finding::UnitDrift),
                };
            };
            if contents(facts, 1) != Some(installed) {
                return Some(Finding::UnitDrift);
            }
            let active = matches!(
                &facts[3],
                Fact::Unit(unit) if unit.active_state == "active" || unit.active_state == "reloading"
            );
            (*must_be_active && !active).then_some(Finding::UnitInactive)
        }
        Target::PathExists { .. } => {
            (path_facts(facts, 0).kind == Kind::Missing).then_some(Finding::RuntimeMissing)
        }
    }
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
            must_be_active,
            ..
        } => {
            let mut actions = Vec::new();
            if finding != Finding::UnitInactive {
                let wanted = contents(facts, 1).ok_or(Unfixable::MissingRuntime)?;
                actions.push(Action::InstallUnit {
                    unit: (*name).to_string(),
                    contents: Arc::from(wanted),
                    expect: path_facts(facts, 2)
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

    fn title(&self) -> Cow<'static, str> {
        Cow::Owned(self.label.clone())
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

struct RestartIfStale;

impl StepSpec for RestartIfStale {
    fn key(&self) -> Cow<'static, str> {
        NIX_DAEMON_SERVICE_UNIT.into()
    }

    fn title(&self) -> Cow<'static, str> {
        "restart the nix daemon if its configuration changed".into()
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

pub fn repair_steps(targets: Vec<Target<'_>>, request: &str) -> Vec<Box<dyn StepSpec>> {
    let mut steps = target_steps(targets, request);
    steps.push(Box::new(RestartIfStale));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The binding between what an inspection found and what repair can do about it.
    #[test]
    fn a_finding_says_whether_repair_can_reconcile_it() {
        assert_eq!(
            Finding::NotADirectory.unfixable(),
            Some(Unfixable::NotADirectory)
        );
        assert_eq!(
            Finding::NoSuchUser.unfixable(),
            Some(Unfixable::MissingUser)
        );
        assert_eq!(
            Finding::RuntimeMissing.unfixable(),
            Some(Unfixable::MissingRuntime)
        );
    }

    #[test]
    fn everything_repair_reconciles_is_bound_to_no_reason() {
        for finding in [
            Finding::Missing,
            Finding::Unreadable {
                kind: std::io::ErrorKind::PermissionDenied,
            },
            Finding::Mode {
                actual: 0o700,
                expected: 0o755,
            },
            Finding::Owner {
                actual: (0, 0),
                expected: (1000, 1000),
            },
            Finding::ContentDrift,
            Finding::GroupMissing,
            Finding::GroupGid {
                actual: 1,
                expected: 30_000,
            },
            Finding::NotAMember { group: "mix-users" },
            Finding::UserMissing,
            Finding::UserIds {
                actual: (1, 1),
                expected: (30_000, 30_000),
            },
            Finding::UnitMissing,
            Finding::UnitDrift,
            Finding::UnitInactive,
        ] {
            assert_eq!(finding.unfixable(), None, "{finding:?}");
        }
    }
}
