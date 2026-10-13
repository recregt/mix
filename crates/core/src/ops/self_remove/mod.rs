use std::borrow::Cow;
use std::path::PathBuf;

use crate::declared::identity::{
    InvokingUser, MIX_USERS_GROUP, NIXBLD_GROUP, NIXBLD_USER_COUNT, user_name,
};
use crate::declared::paths::{
    HOME_TRACES, MIX_BIN_DIR, MIX_DAEMON_SERVICE_DEST, MIX_DAEMON_SERVICE_UNIT,
    MIX_DAEMON_SOCKET_DEST, MIX_DAEMON_SOCKET_UNIT, NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST,
    NIX_DAEMON_SERVICE_UNIT, NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_UNIT, NIX_ENTRIES,
    POLICY_FILE, PROFILE_SNIPPET_DEST, join,
};
use crate::effect::{Action, Fact, Failure, FileId, PathFacts, Query, UnitFacts};
use crate::run::{StepSpec, Title};
use mix_events::v1::Verb;

const UNITS: [&str; 4] = [
    MIX_DAEMON_SOCKET_UNIT,
    MIX_DAEMON_SERVICE_UNIT,
    NIX_DAEMON_SOCKET_UNIT,
    NIX_DAEMON_SERVICE_UNIT,
];

const UNIT_FILES: [&str; 4] = [
    MIX_DAEMON_SOCKET_DEST,
    MIX_DAEMON_SERVICE_DEST,
    NIX_DAEMON_SOCKET_DEST,
    NIX_DAEMON_SERVICE_DEST,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub users: Vec<InvokingUser>,
}

fn path(fact: &Fact) -> &PathFacts {
    match fact {
        Fact::Path(facts) => facts,
        other => unreachable!("a path query was answered with {other:?}"),
    }
}

fn set_aside(path: impl Into<PathBuf>, found: &PathFacts) -> Option<Action> {
    found.id.map(|expect| Action::SetAside {
        path: path.into(),
        expect,
    })
}

fn set_aside_each(paths: &[PathBuf], facts: &[Fact]) -> Vec<Action> {
    paths
        .iter()
        .zip(facts)
        .filter_map(|(at, fact)| set_aside(at.clone(), path(fact)))
        .collect()
}

struct RemoveHome {
    user: InvokingUser,
}

impl RemoveHome {
    fn traces(&self) -> Vec<PathBuf> {
        HOME_TRACES
            .iter()
            .map(|trace| join(&self.user.home, trace))
            .collect()
    }
}

impl StepSpec for RemoveHome {
    fn key(&self) -> Cow<'static, str> {
        format!("remove-home:{}", self.user.name).into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Removing, self.user.home.display().to_string())
    }

    fn queries(&self) -> Vec<Query> {
        std::iter::once(Query::Linked(self.user.clone()))
            .chain(self.traces().into_iter().map(Query::Path))
            .collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let traces = self.traces();
        let linked: &[(PathBuf, FileId)] = match &facts[0] {
            Fact::Linked(linked) => linked,
            other => unreachable!("a linked query was answered with {other:?}"),
        };
        let mut actions: Vec<Action> = linked
            .iter()
            .filter(|(at, _)| !traces.iter().any(|trace| at.starts_with(trace)))
            .map(|(at, id)| Action::SetAside {
                path: at.clone(),
                expect: *id,
            })
            .collect();
        actions.extend(set_aside_each(&traces, &facts[1..]));
        Ok(actions)
    }
}

struct StopServices;

impl StepSpec for StopServices {
    fn key(&self) -> Cow<'static, str> {
        "stop-services".into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Removing, "mix and Nix services")
    }

    fn queries(&self) -> Vec<Query> {
        UNITS
            .iter()
            .map(|unit| Query::Unit((*unit).to_string()))
            .chain(UNIT_FILES.iter().map(|file| Query::Path(file.into())))
            .collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let mut actions = Vec::new();
        for (unit, fact) in UNITS.iter().zip(facts) {
            let state: &UnitFacts = match fact {
                Fact::Unit(state) => state,
                other => unreachable!("a unit query was answered with {other:?}"),
            };
            if state.active_state == "active" {
                actions.push(Action::StopUnit {
                    unit: (*unit).to_string(),
                });
            }
            if state.enabled() {
                actions.push(Action::DisableUnit {
                    unit: (*unit).to_string(),
                });
            }
        }
        let files: Vec<PathBuf> = UNIT_FILES.iter().map(PathBuf::from).collect();
        let removed = set_aside_each(&files, &facts[UNITS.len()..]);
        if !removed.is_empty() {
            actions.extend(removed);
            actions.push(Action::DaemonReload);
        }
        Ok(actions)
    }
}

fn policy_dir() -> &'static str {
    POLICY_FILE
        .rsplit_once('/')
        .map_or(POLICY_FILE, |(dir, _)| dir)
}

struct RemoveMachine;

impl RemoveMachine {
    fn paths() -> Vec<PathBuf> {
        NIX_ENTRIES
            .iter()
            .chain(&[
                NIX_CONF_DEST,
                PROFILE_SNIPPET_DEST,
                policy_dir(),
                MIX_BIN_DIR,
            ])
            .map(PathBuf::from)
            .collect()
    }
}

impl StepSpec for RemoveMachine {
    fn key(&self) -> Cow<'static, str> {
        "remove-machine".into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Removing, "/nix, /etc and /var/lib/mix")
    }

    fn queries(&self) -> Vec<Query> {
        Self::paths().into_iter().map(Query::Path).collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok(set_aside_each(&Self::paths(), facts))
    }
}

struct RemoveAccounts;

impl StepSpec for RemoveAccounts {
    fn key(&self) -> Cow<'static, str> {
        "remove-accounts".into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Removing, "build users and groups")
    }

    fn queries(&self) -> Vec<Query> {
        (1..=NIXBLD_USER_COUNT)
            .map(|n| Query::User(user_name(n).into_owned()))
            .chain([NIXBLD_GROUP, MIX_USERS_GROUP].map(|group| Query::Group(group.to_string())))
            .collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let mut actions = Vec::new();
        for (n, fact) in (1..=NIXBLD_USER_COUNT).zip(facts) {
            if let Fact::User(Some(user)) = fact {
                actions.push(Action::DeleteUser {
                    name: user_name(n).into_owned(),
                    expect: (user.uid, user.gid),
                    comment: user.comment.clone(),
                });
            }
        }
        let groups = &facts[NIXBLD_USER_COUNT as usize..];
        for (name, fact) in [NIXBLD_GROUP, MIX_USERS_GROUP].iter().zip(groups) {
            if let Fact::Group(Some(group)) = fact {
                actions.push(Action::DeleteGroup {
                    name: (*name).to_string(),
                    expect: group.gid,
                });
            }
        }
        Ok(actions)
    }
}

pub fn steps(settings: &Settings) -> Vec<Box<dyn StepSpec>> {
    let mut steps: Vec<Box<dyn StepSpec>> = settings
        .users
        .iter()
        .map(|user| Box::new(RemoveHome { user: user.clone() }) as Box<dyn StepSpec>)
        .collect();
    steps.push(Box::new(StopServices));
    steps.push(Box::new(RemoveMachine));
    steps.push(Box::new(RemoveAccounts));
    steps
}

#[cfg(test)]
mod tests;
