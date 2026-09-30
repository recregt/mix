#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use std::fmt::Write as _;

use mix_events::v1::{Action, Journaled, Observation, PathKind, journaled, observation};

fn query(query: &observation::Query) -> (&'static str, &str) {
    use observation::Query;

    match query {
        Query::Path(path) => ("path", path),
        Query::Contents(path) => ("contents of", path),
        Query::Group(name) => ("group", name),
        Query::User(name) => ("user", name),
        Query::Unit(name) => ("unit", name),
        Query::Profile(name) => ("profile of", name),
        Query::TreeOwner(path) => ("owner of", path),
    }
}

fn fact(out: &mut String, fact: &observation::Fact) {
    use observation::Fact;

    let _ = match fact {
        Fact::PathFact(path) => {
            let kind = match path.kind() {
                PathKind::Missing => return out.push_str("missing"),
                PathKind::Unspecified => return out.push_str("unknown"),
                PathKind::Unreadable => {
                    let _ = write!(out, "unreadable ({})", path.unreadable);
                    return;
                }
                PathKind::Directory => "directory",
                PathKind::File => "file",
                PathKind::Symlink => "symlink",
                PathKind::Other => "other",
            };
            write!(
                out,
                "{kind}, mode {:o}, owner {}:{}",
                path.mode, path.uid, path.gid
            )
        }
        Fact::ContentsFact(contents) if contents.present => {
            write!(out, "{} bytes", contents.length)
        }
        Fact::ContentsFact(_) => write!(out, "absent"),
        Fact::GroupFact(group) if group.exists => write!(
            out,
            "gid {}, members {}",
            group.gid,
            group.members.join(", ")
        ),
        Fact::UserFact(user) if user.exists => {
            write!(
                out,
                "uid {}, gid {}, home {}",
                user.uid, user.gid, user.home
            )
        }
        Fact::GroupFact(_) | Fact::UserFact(_) => write!(out, "absent"),
        Fact::UnitFact(unit) => write!(
            out,
            "{} {} {}",
            unit.load_state, unit.active_state, unit.file_state
        ),
        Fact::ProfileFact(profile) => match profile.active {
            Some(active) => write!(
                out,
                "{} generations, active {active}",
                profile.generations.len()
            ),
            None => write!(
                out,
                "{} generations, none active",
                profile.generations.len()
            ),
        },
        Fact::TreeOwnerFact(owner) => match owner.uid {
            Some(uid) => write!(out, "uid {uid}"),
            None => write!(out, "none"),
        },
    };
}

pub(crate) fn observation(observation: &Observation) -> String {
    let mut out = String::new();
    if let Some(asked) = &observation.query {
        let (what, subject) = query(asked);
        let _ = write!(out, "{what} {subject}: ");
    }
    match &observation.fact {
        Some(found) => fact(&mut out, found),
        None => out.push_str("unknown"),
    }
    out
}

fn undo(out: &mut String, undo: &[Action]) {
    for (index, action) in undo.iter().enumerate() {
        out.push_str(if index == 0 { ": undo " } else { ", " });
        out.push_str(
            action
                .operation()
                .as_str_name()
                .trim_start_matches("OPERATION_"),
        );
        out.push(' ');
        out.push_str(&action.subject);
    }
}

pub(crate) fn record(journaled: &Journaled) -> String {
    use journaled::Record;

    let mut out = String::new();
    match &journaled.record {
        Some(Record::Began(began)) => {
            let _ = write!(out, "began {}", began.request);
        }
        Some(Record::Prepared(step)) => {
            let _ = write!(out, "prepared {}", step.seq);
            undo(&mut out, &step.undo);
        }
        Some(Record::Done(step)) => {
            let _ = write!(out, "done {}", step.seq);
        }
        Some(Record::Settled(step)) => {
            let _ = write!(out, "settled {}", step.seq);
            undo(&mut out, &step.undo);
        }
        Some(Record::Failed(step)) => {
            let _ = write!(out, "failed {}", step.seq);
        }
        Some(Record::Reverted(action)) => {
            out.push_str("reverted");
            undo(&mut out, std::slice::from_ref(action));
        }
        Some(Record::Committing(_)) => out.push_str("committing"),
        Some(Record::Ended(_)) => out.push_str("ended"),
        None => out.push_str("unknown"),
    }
    out
}

#[cfg(test)]
mod tests {
    use mix_events::v1::{ContentsFact, Operation, PathFact, Sequenced};

    use super::*;

    #[test]
    fn an_observation_reads_as_what_was_asked_and_what_was_found() {
        let found = Observation {
            query: Some(observation::Query::Path("/nix".into())),
            fact: Some(observation::Fact::PathFact(PathFact {
                kind: PathKind::Directory as i32,
                mode: 0o755,
                ..PathFact::default()
            })),
        };
        assert_eq!(
            observation(&found),
            "path /nix: directory, mode 755, owner 0:0"
        );
        let absent = Observation {
            query: Some(observation::Query::Contents("/etc/nix/nix.conf".into())),
            fact: Some(observation::Fact::ContentsFact(ContentsFact::default())),
        };
        assert_eq!(
            observation(&absent),
            "contents of /etc/nix/nix.conf: absent"
        );
    }

    #[test]
    fn a_record_names_its_sequence_and_the_undo_it_holds() {
        let prepared = Journaled {
            record: Some(journaled::Record::Prepared(Sequenced {
                seq: 3,
                undo: vec![Action {
                    operation: Operation::RemoveCreated as i32,
                    subject: "/nix/var".into(),
                }],
            })),
        };
        assert_eq!(
            record(&prepared),
            "prepared 3: undo REMOVE_CREATED /nix/var"
        );
    }
}
