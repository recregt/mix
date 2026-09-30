use std::path::PathBuf;
use std::sync::Arc;

use mix_events::v1::{Operation, journaled, observation};

use super::*;
use crate::action::{Digest, GroupFacts};

#[test]
fn each_query_is_paired_with_the_fact_it_found() {
    let queries = [
        Query::Path(PathBuf::from("/nix")),
        Query::Contents(PathBuf::from("/etc/nix/nix.conf")),
        Query::Group("nixbld".into()),
    ];
    let facts = [
        Fact::Path(PathFacts {
            kind: Kind::Directory,
            mode: 0o755,
            owner: (0, 0),
            id: Some(FileId {
                dev: 1,
                ino: 2,
                born: Some((10, 20)),
            }),
            digest: Some(Digest([7; 32])),
            changed: None,
        }),
        Fact::Contents(Some(Arc::from(&b"trusted-users = root"[..]))),
        Fact::Group(Some(GroupFacts {
            gid: 30000,
            members: vec!["nixbld1".into()],
        })),
    ];

    let observed = observed(&queries, &facts);

    assert_eq!(observed.observations.len(), 3);
    let first = &observed.observations[0];
    assert_eq!(first.query, Some(observation::Query::Path("/nix".into())));
    let Some(observation::Fact::PathFact(path)) = &first.fact else {
        panic!("{first:?}")
    };
    assert_eq!(path.kind(), PathKind::Directory);
    assert_eq!(path.mode, 0o755);
    assert_eq!(path.digest, vec![7; 32]);
    assert_eq!(path.id.as_ref().unwrap().born.unwrap().seconds, 10);
    assert_eq!(
        observed.observations[1].fact,
        Some(observation::Fact::ContentsFact(ContentsFact {
            present: true,
            length: 20
        }))
    );
    assert!(matches!(
        &observed.observations[2].fact,
        Some(observation::Fact::GroupFact(group)) if group.exists && group.gid == 30000
    ));
}

#[test]
fn a_journal_record_keeps_its_sequence_and_the_undo_it_announced() {
    let record = Record::Prepared {
        seq: 3,
        undo: vec![Action::RemoveCreated {
            path: PathBuf::from("/nix/var"),
            expect: FileId {
                dev: 1,
                ino: 2,
                born: None,
            },
        }],
    };

    let Some(journaled::Record::Prepared(prepared)) = journaled(&record).record else {
        panic!("not a prepared record")
    };

    assert_eq!(prepared.seq, 3);
    assert_eq!(prepared.undo[0].operation(), Operation::RemoveCreated);
    assert_eq!(prepared.undo[0].subject, "/nix/var");
}
