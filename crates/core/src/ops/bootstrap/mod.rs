pub mod system;

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::declared::identity::{MIX_USERS_GROUP, NIXBLD_GROUP, NIXBLD_USER_COUNT, user_name};
use crate::declared::paths::{
    NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_UNIT, NIX_DAEMON_SOCKET_DEST,
    NIX_DAEMON_SOCKET_UNIT, POLICY_FILE, PROFILE_SNIPPET_DEST,
};
use crate::declared::policy::Policy;
use crate::declared::targets::{Intent, Runtime, UserConfig, tree_for};
use crate::effect::{Action, Fact, Failure, GroupFacts, PathFacts, Query, UnitFacts, UserFacts};
use crate::ops::health::reconcile_steps;
use crate::ops::removal::set_aside_units;
use crate::run::{StepSpec, Title};
use mix_events::v1::Verb;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub policy: Policy,
    pub user: Option<UserConfig>,
    pub force: bool,
    pub runtime: Runtime,
    pub request: String,
}

struct Facts<'a>(pub(crate) &'a [Fact]);

impl Facts<'_> {
    fn path(&self, index: usize) -> &PathFacts {
        match &self.0[index] {
            Fact::Path(facts) => facts,
            other => unreachable!("a path query was answered with {other:?}"),
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

fn set_aside(path: &Path, facts: &PathFacts) -> Option<Action> {
    facts.id.map(|expect| Action::SetAside {
        path: path.to_path_buf(),
        expect,
    })
}

struct RemoveExistingInstallation;

const UNITS: [&str; 2] = [NIX_DAEMON_SOCKET_UNIT, NIX_DAEMON_SERVICE_UNIT];
const UNIT_FILES: [&str; 2] = [NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SOCKET_DEST];
const SET_ASIDE: [&str; 4] = [NIX_CONF_DEST, PROFILE_SNIPPET_DEST, POLICY_FILE, "/nix"];

impl StepSpec for RemoveExistingInstallation {
    fn key(&self) -> Cow<'static, str> {
        "remove-existing-installation".into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Removing, "existing installation")
    }

    fn queries(&self) -> Vec<Query> {
        let mut queries: Vec<Query> = UNITS
            .iter()
            .map(|unit| Query::Unit(unit.to_string()))
            .collect();
        queries.extend(UNITS.iter().map(|unit| Query::UnitLinks(unit.to_string())));
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
        for (index, unit) in UNITS.iter().enumerate() {
            if facts.unit(index).active_state == "active" {
                actions.push(Action::StopUnit {
                    unit: unit.to_string(),
                });
            }
        }
        let files = 2 * UNITS.len();
        let unit_files: Vec<PathBuf> = UNIT_FILES.iter().map(PathBuf::from).collect();
        actions.extend(set_aside_units(
            &facts.0[UNITS.len()..files],
            &unit_files,
            &facts.0[files..files + UNIT_FILES.len()],
        ));
        let users = files + UNIT_FILES.len();
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

pub fn stale_restart(service: &UnitFacts, configured: Option<(i64, u32)>) -> Option<Action> {
    (service.active_state == "active"
        && matches!(
            (service.active_since, configured),
            (Some(since), Some(configured)) if configured > since
        ))
    .then(|| Action::RestartUnit {
        unit: NIX_DAEMON_SERVICE_UNIT.to_string(),
    })
}

pub fn steps(settings: &Settings) -> Vec<Box<dyn StepSpec>> {
    let before: Vec<Box<dyn StepSpec>> = if settings.force {
        vec![Box::new(RemoveExistingInstallation)]
    } else {
        Vec::new()
    };
    let intent = Intent {
        runtime: Some(&settings.runtime),
        tidy: false,
        ..Intent::machine(settings.user.as_ref(), &settings.policy)
    };
    reconcile_steps(
        before,
        tree_for(&intent),
        &settings.request,
        Verb::Configuring,
    )
}

#[cfg(test)]
mod tests;
