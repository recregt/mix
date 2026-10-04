use mix_events::Timestamp;
use mix_events::v1::{
    Began, ContentsFact, FileIdentity, GroupFact, Journaled, Observation, Observed, PathFact,
    PathKind, PathsFact, ProfileFact, ProgramFact, RepositoryFact, Sequenced, StrangerFact,
    TreeOwnerFact, UnitFact, UserFact, journaled, observation,
};

use crate::action::{Action, Fact, FileId, Kind, PathFacts, Query};
use crate::journal::Record;
use crate::paths::repository_dir;
use crate::plan::describe;

pub fn observed(queries: &[Query], facts: &[Fact]) -> Observed {
    Observed {
        observations: queries
            .iter()
            .zip(facts)
            .map(|(query, fact)| Observation {
                query: Some(query_of(query)),
                fact: Some(fact_of(fact)),
            })
            .collect(),
    }
}

pub fn journaled(record: &Record) -> Journaled {
    use journaled::Record as Wire;

    let sequenced = |seq: &u64, undo: &[Action]| Sequenced {
        seq: *seq,
        undo: undo.iter().map(action).collect(),
    };
    Journaled {
        record: Some(match record {
            Record::Began { request } => Wire::Began(Began {
                request: request.clone(),
            }),
            Record::Prepared { seq, undo } => Wire::Prepared(sequenced(seq, undo)),
            Record::Done { seq } => Wire::Done(sequenced(seq, &[])),
            Record::Settled { seq, undo } => Wire::Settled(sequenced(seq, undo)),
            Record::Failed { seq } => Wire::Failed(sequenced(seq, &[])),
            Record::Reverted { action: reverted } => Wire::Reverted(action(reverted)),
            Record::Committing => Wire::Committing(Default::default()),
            Record::Ended => Wire::Ended(Default::default()),
        }),
    }
}

fn action(action: &Action) -> mix_events::v1::Action {
    let (operation, subject) = describe(action);
    mix_events::v1::Action {
        operation: operation as i32,
        subject,
    }
}

fn moment((seconds, nanos): (i64, u32)) -> Timestamp {
    Timestamp {
        seconds,
        nanos: i32::try_from(nanos).unwrap_or(i32::MAX),
    }
}

fn query_of(query: &Query) -> observation::Query {
    use observation::Query as Wire;

    let path = |path: &std::path::Path| path.display().to_string();
    match query {
        Query::Path(at) => Wire::Path(path(at)),
        Query::Contents(at) => Wire::Contents(path(at)),
        Query::Group(name) => Wire::Group(name.clone()),
        Query::User(name) => Wire::User(name.clone()),
        Query::Unit(name) => Wire::Unit(name.clone()),
        Query::Profile(user) => Wire::Profile(user.name.clone()),
        Query::TreeOwner(at) => Wire::TreeOwner(path(at)),
        Query::Repository(user) => Wire::Repository(path(&repository_dir(&user.home))),
        Query::ActiveList(user) => {
            Wire::Contents(path(&crate::paths::active_list_path(&user.home)))
        }
        Query::Journals(at) => Wire::Journals(path(at)),
        Query::Leftovers(at) => Wire::Leftovers(path(at)),
        Query::Strangers { path: at, .. } => Wire::Strangers(path(at)),
        Query::Clobbered(user) => Wire::Clobbered(user.name.clone()),
        Query::Program { path: at, .. } => Wire::Program(path(at)),
    }
}

fn identity(id: &FileId) -> FileIdentity {
    FileIdentity {
        dev: id.dev,
        ino: id.ino,
        born: id.born.map(moment),
    }
}

fn path_fact(facts: &PathFacts) -> PathFact {
    let (kind, unreadable) = match facts.kind {
        Kind::Missing => (PathKind::Missing, String::new()),
        Kind::Unreadable(kind) => (PathKind::Unreadable, format!("{kind:?}")),
        Kind::Directory => (PathKind::Directory, String::new()),
        Kind::File => (PathKind::File, String::new()),
        Kind::Symlink => (PathKind::Symlink, String::new()),
        Kind::Other => (PathKind::Other, String::new()),
    };
    PathFact {
        kind: kind as i32,
        unreadable,
        mode: facts.mode,
        uid: facts.owner.0,
        gid: facts.owner.1,
        id: facts.id.as_ref().map(identity),
        digest: facts
            .digest
            .map(|digest| digest.0.to_vec())
            .unwrap_or_default(),
        changed: facts.changed.map(moment),
    }
}

fn fact_of(fact: &Fact) -> observation::Fact {
    use observation::Fact as Wire;

    match fact {
        Fact::Path(facts) => Wire::PathFact(path_fact(facts)),
        Fact::Contents(contents) => Wire::ContentsFact(ContentsFact {
            present: contents.is_some(),
            length: contents.as_ref().map_or(0, |bytes| bytes.len() as u64),
        }),
        Fact::Group(group) => Wire::GroupFact(match group {
            Some(group) => GroupFact {
                exists: true,
                gid: group.gid,
                members: group.members.clone(),
            },
            None => GroupFact::default(),
        }),
        Fact::User(user) => Wire::UserFact(match user {
            Some(user) => UserFact {
                exists: true,
                uid: user.uid,
                gid: user.gid,
                home: user.home.display().to_string(),
                shell: user.shell.display().to_string(),
                comment: user.comment.clone(),
            },
            None => UserFact::default(),
        }),
        Fact::Unit(unit) => Wire::UnitFact(UnitFact {
            load_state: unit.load_state.clone(),
            active_state: unit.active_state.clone(),
            file_state: unit.file_state.clone(),
            needs_reload: unit.needs_reload,
            active_since: unit.active_since.map(moment),
        }),
        Fact::Profile(profile) => Wire::ProfileFact(ProfileFact {
            generations: profile.generations.clone(),
            active: profile.active,
            dangling: profile.dangling.clone(),
        }),
        Fact::TreeOwner(uid) => Wire::TreeOwnerFact(TreeOwnerFact { uid: *uid }),
        Fact::Repository { intact, recorded } => Wire::RepositoryFact(RepositoryFact {
            intact: *intact,
            recorded: *recorded,
        }),
        Fact::Journals(abandoned) => Wire::JournalsFact(PathsFact {
            paths: abandoned
                .iter()
                .map(|request| request.request.clone())
                .collect(),
        }),
        Fact::Leftovers(found) => Wire::LeftoversFact(PathsFact {
            paths: found
                .iter()
                .map(|(at, _)| at.display().to_string())
                .collect(),
        }),
        Fact::Stranger(found) => Wire::StrangerFact(match found {
            Some((at, (uid, gid))) => StrangerFact {
                path: Some(at.display().to_string()),
                uid: *uid,
                gid: *gid,
            },
            None => StrangerFact::default(),
        }),
        Fact::Program(program) => Wire::ProgramFact(ProgramFact { same: program.same }),
        Fact::Clobbered(found) => Wire::ClobberedFact(PathsFact {
            paths: found.iter().map(|at| at.display().to_string()).collect(),
        }),
    }
}

#[cfg(test)]
mod tests;
