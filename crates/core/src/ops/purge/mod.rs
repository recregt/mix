use std::borrow::Cow;
use std::path::PathBuf;

use crate::declared::identity::{
    InvokingUser, MIX_USERS_GROUP, NIXBLD_GROUP, NIXBLD_USER_COUNT, user_name,
};
use crate::declared::paths::{
    LOGIN_DEFS, MIX_BIN_DIR, MIX_DAEMON_SERVICE_DEST, MIX_DAEMON_SERVICE_UNIT,
    MIX_DAEMON_SOCKET_DEST, MIX_DAEMON_SOCKET_UNIT, MIX_RUN_DIR, NIX_CONF_DEST,
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_UNIT, NIX_DAEMON_SOCKET_DEST,
    NIX_DAEMON_SOCKET_UNIT, NIX_ENTRIES, POLICY_FILE, PROFILE_SNIPPET_DEST, mix_state_dir,
};
use crate::effect::{Action, Fact, Failure, Query, UnitFacts};
use crate::ops::removal::{RemoveHome, path, set_aside_each, set_aside_units};
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

fn unit_facts(fact: &Fact) -> &UnitFacts {
    match fact {
        Fact::Unit(state) => state,
        other => unreachable!("a unit query was answered with {other:?}"),
    }
}

fn unit_queries() -> impl Iterator<Item = Query> {
    UNITS.iter().map(|unit| Query::Unit((*unit).to_string()))
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
        unit_queries().collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok(UNITS
            .iter()
            .zip(facts)
            .filter(|(_, fact)| unit_facts(fact).active_state == "active")
            .map(|(unit, _)| Action::StopUnit {
                unit: (*unit).to_string(),
            })
            .collect())
    }
}

struct RemoveUnits;

impl StepSpec for RemoveUnits {
    fn key(&self) -> Cow<'static, str> {
        "remove-units".into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Removing, "mix and Nix unit files")
    }

    fn queries(&self) -> Vec<Query> {
        unit_queries()
            .chain(
                UNITS
                    .iter()
                    .map(|unit| Query::UnitLinks((*unit).to_string())),
            )
            .chain(UNIT_FILES.iter().map(|file| Query::Path(file.into())))
            .collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let mut actions: Vec<Action> = UNITS
            .iter()
            .zip(facts)
            .filter(|(_, fact)| unit_facts(fact).active_state == "active")
            .map(|(unit, _)| Action::StopUnit {
                unit: (*unit).to_string(),
            })
            .collect();
        let files: Vec<PathBuf> = UNIT_FILES.iter().map(PathBuf::from).collect();
        let links = &facts[UNITS.len()..2 * UNITS.len()];
        actions.extend(set_aside_units(links, &files, &facts[2 * UNITS.len()..]));
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
                MIX_RUN_DIR,
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
        Title::new(Verb::Removing, "/nix, /etc, /run/mix and /var/lib/mix")
    }

    fn queries(&self) -> Vec<Query> {
        Self::paths().into_iter().map(Query::Path).collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let (nix, rest) = facts.split_at(NIX_ENTRIES.len());
        let paths = Self::paths();
        let mut actions: Vec<Action> = paths
            .iter()
            .zip(nix)
            .filter_map(|(at, fact)| {
                path(fact).id.map(|expect| Action::Discard {
                    path: at.clone(),
                    expect,
                })
            })
            .collect();
        actions.extend(set_aside_each(&paths[NIX_ENTRIES.len()..], rest));
        Ok(actions)
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

pub fn discovery() -> Vec<Query> {
    vec![
        Query::Group(MIX_USERS_GROUP.to_string()),
        Query::Group(NIXBLD_GROUP.to_string()),
        Query::Accounts,
        Query::Contents(LOGIN_DEFS.into()),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    members: Vec<InvokingUser>,
    others: Vec<InvokingUser>,
}

impl Candidates {
    pub fn of(facts: &[Fact]) -> Self {
        let [
            Fact::Group(members),
            Fact::Group(builders),
            Fact::Accounts(accounts),
            Fact::Contents(login_defs),
        ] = facts
        else {
            unreachable!("the discovery queries are answered in order: {facts:?}")
        };
        let account = |name: &str, found: &crate::effect::UserFacts| InvokingUser {
            uid: found.uid,
            gid: found.gid,
            name: name.to_string(),
            home: found.home.clone(),
        };
        let usable = |name: &str, found: &crate::effect::UserFacts| {
            !name.contains('"') && found.home.is_absolute() && found.home.to_str().is_some()
        };
        let members: Vec<InvokingUser> = members
            .iter()
            .flat_map(|group| &group.members)
            .filter_map(|member| {
                accounts
                    .iter()
                    .find(|(name, _)| name == member)
                    .filter(|(name, found)| usable(name, found))
                    .map(|(name, found)| account(name, found))
            })
            .collect();
        let regular = uid_range(&String::from_utf8_lossy(
            login_defs.as_deref().unwrap_or_default(),
        ));
        let builders = builders.as_ref().map(|group| group.gid);
        let others = accounts
            .iter()
            .filter(|(name, found)| {
                regular.contains(&found.uid)
                    && Some(found.gid) != builders
                    && usable(name, found)
                    && !members.iter().any(|member| member.name == *name)
            })
            .map(|(name, found)| account(name, found))
            .collect();
        Self { members, others }
    }

    pub fn queries(&self) -> Vec<Query> {
        self.others
            .iter()
            .map(|other| Query::Path(mix_state_dir(&other.home)))
            .collect()
    }

    pub fn holders(self, states: &[Fact]) -> Vec<InvokingUser> {
        let mut holders = self.members;
        holders.extend(
            self.others
                .into_iter()
                .zip(states)
                .filter(|(_, state)| matches!(state, Fact::Path(found) if found.id.is_some()))
                .map(|(other, _)| other),
        );
        holders
    }
}

fn uid_range(login_defs: &str) -> std::ops::RangeInclusive<u32> {
    let setting = |key: &str| {
        login_defs.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some(key))
                .then(|| words.next()?.parse::<u32>().ok())
                .flatten()
        })
    };
    setting("UID_MIN").unwrap_or(1000)..=setting("UID_MAX").unwrap_or(60000)
}

pub fn steps(settings: &Settings) -> Vec<Box<dyn StepSpec>> {
    let mut steps: Vec<Box<dyn StepSpec>> = vec![Box::new(StopServices)];
    steps.extend(
        settings
            .users
            .iter()
            .map(|user| Box::new(RemoveHome::new(user.clone())) as Box<dyn StepSpec>),
    );
    steps.push(Box::new(RemoveUnits));
    steps.push(Box::new(RemoveMachine));
    steps.push(Box::new(RemoveAccounts));
    steps
}

#[cfg(test)]
mod tests;
