use std::borrow::Cow;
use std::path::PathBuf;

use crate::declared::identity::InvokingUser;
use crate::declared::paths::{HOME_TRACES, join};
use crate::effect::{Action, Fact, Failure, FileId, PathFacts, Query};
use crate::run::{StepSpec, Title};
use mix_events::v1::Verb;

pub(super) fn path(fact: &Fact) -> &PathFacts {
    match fact {
        Fact::Path(facts) => facts,
        other => unreachable!("a path query was answered with {other:?}"),
    }
}

pub(super) fn set_aside(path: impl Into<PathBuf>, found: &PathFacts) -> Option<Action> {
    found.id.map(|expect| Action::SetAside {
        path: path.into(),
        expect,
    })
}

pub(super) fn set_aside_each(paths: &[PathBuf], facts: &[Fact]) -> Vec<Action> {
    paths
        .iter()
        .zip(facts)
        .filter_map(|(at, fact)| set_aside(at.clone(), path(fact)))
        .collect()
}

pub(super) struct RemoveHome {
    user: InvokingUser,
}

impl RemoveHome {
    pub(super) fn new(user: InvokingUser) -> Self {
        Self { user }
    }

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
