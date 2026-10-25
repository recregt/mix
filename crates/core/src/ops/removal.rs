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

fn unit_links(fact: &Fact) -> &[(PathBuf, FileId)] {
    match fact {
        Fact::UnitLinks(links) => links,
        other => unreachable!("a unit links query was answered with {other:?}"),
    }
}

pub(super) fn set_aside_units(links: &[Fact], files: &[PathBuf], found: &[Fact]) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut moved: Vec<&PathBuf> = Vec::new();
    for (link, expect) in links.iter().flat_map(unit_links) {
        if !moved.contains(&link) {
            actions.push(Action::SetAside {
                path: link.clone(),
                expect: *expect,
            });
            moved.push(link);
        }
    }
    for (file, fact) in files.iter().zip(found) {
        if !moved.contains(&file)
            && let Some(action) = set_aside(file.clone(), path(fact))
        {
            actions.push(action);
            moved.push(file);
        }
    }
    if !actions.is_empty() {
        actions.push(Action::DaemonReload);
    }
    actions
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
        Title::new(
            Verb::Removing,
            format!("mix from {}", self.user.home.display()),
        )
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
