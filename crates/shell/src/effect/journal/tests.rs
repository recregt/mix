#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::sync::Arc;

use mix_core::effect::{Expect, Fact, Query};

use super::*;
use crate::effect::files::Files;

fn listing(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let contents = path.is_file().then(|| std::fs::read(&path).unwrap());
            if path.is_dir() {
                pending.push(path.clone());
            }
            found.push((path.strip_prefix(root).unwrap().to_path_buf(), contents));
        }
    }
    found.sort();
    found
}

fn machine() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("etc")).unwrap();
    std::fs::write(root.path().join("etc/nix.conf"), "legacy\n").unwrap();
    root
}

fn id_of(files: &Files, path: &str) -> mix_core::effect::FileId {
    match files.observe(&Query::Path(path.into())) {
        Some(Fact::Path(facts)) => facts.id.unwrap(),
        other => panic!("{other:?}"),
    }
}

fn changes(files: &Files) -> Vec<Action> {
    vec![
        Action::CreateDir {
            path: "/nix".into(),
            mode: 0o755,
            owner: None,
        },
        Action::CreateDir {
            path: "/nix/var".into(),
            mode: 0o755,
            owner: None,
        },
        Action::PutFile {
            path: "/nix/.mix-managed".into(),
            contents: Arc::from(&b""[..]),
            mode: 0o644,
            owner: None,
            expect: Expect::Absent,
        },
        Action::PutFile {
            path: "/etc/nix.conf".into(),
            contents: Arc::from(&b"trusted-users = root\n"[..]),
            mode: 0o644,
            owner: None,
            expect: Expect::Present(id_of(files, "/etc/nix.conf")),
        },
    ]
}

async fn perform(
    performer: &mut Performer,
    action: &Action,
    prepared: &mut crate::effect::files::Prepared<'_>,
) -> mix_core::effect::Outcome {
    let mut quiet = |_| {};
    performer
        .perform(action, &Scope::root(), &mut quiet, prepared)
        .await
}

#[test]
fn records_are_read_back_and_a_torn_last_line_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = FileJournal::create(dir.path(), "r1").unwrap();
    journal.append(&Record::Done { seq: 0 }).unwrap();
    let path = dir.path().join("r1.ndjson");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"prepared\":{\"seq\":1,\"und")
        .unwrap();

    assert_eq!(
        read(&path).unwrap(),
        [
            Record::Began {
                request: "r1".into()
            },
            Record::Done { seq: 0 }
        ]
    );
    assert_eq!(unfinished(dir.path()), [path]);
}

#[tokio::test]
async fn a_crash_before_or_after_any_change_is_undone_by_the_next_process() {
    let count = changes(&Files::open(machine().path(), "probe").unwrap()).len();
    for crash_at in 0..count {
        for after_change in [false, true] {
            let root = machine();
            let journals = tempfile::tempdir().unwrap();
            let initial = listing(root.path());
            let mut journal = FileJournal::create(journals.path(), "r1").unwrap();
            let mut performer = Performer::new(Files::open(root.path(), "r1").unwrap());
            let actions = changes(&Files::open(root.path(), "probe").unwrap());

            for (seq, action) in actions.iter().enumerate().take(crash_at + 1) {
                let seq = seq as u64;
                let crash = seq as usize == crash_at;
                let mut prepared = |undo: &[Action]| {
                    journal.append(&Record::Prepared {
                        seq,
                        undo: undo.to_vec(),
                    })?;
                    if crash && !after_change {
                        return Err(Failure::Cancelled);
                    }
                    Ok(())
                };
                let outcome = perform(&mut performer, action, &mut prepared).await;
                if crash {
                    assert_eq!(outcome.is_ok(), after_change);
                    break;
                }
                outcome.unwrap();
                journal.append(&Record::Done { seq }).unwrap();
            }
            drop(journal);
            drop(performer);

            let mut next = Performer::new(Files::open(root.path(), "r2").unwrap());
            let recovered = recover_all(journals.path(), &mut next, &Scope::root()).await;

            assert!(
                recovered.failures.is_empty(),
                "crash at {crash_at}, after: {after_change}: {:?}",
                recovered.failures
            );
            assert_eq!(recovered.requests, 1);
            assert_eq!(
                listing(root.path()),
                initial,
                "crash at {crash_at}, after the change: {after_change}"
            );
            assert!(unfinished(journals.path()).is_empty());
        }
    }
}

#[tokio::test]
async fn a_crash_while_committing_is_finished_by_the_next_process() {
    let root = machine();
    let journals = tempfile::tempdir().unwrap();
    let mut journal = FileJournal::create(journals.path(), "r1").unwrap();
    let mut performer = Performer::new(Files::open(root.path(), "r1").unwrap());
    for (seq, action) in changes(&Files::open(root.path(), "probe").unwrap())
        .iter()
        .enumerate()
    {
        let seq = seq as u64;
        let mut prepared = |undo: &[Action]| {
            journal.append(&Record::Prepared {
                seq,
                undo: undo.to_vec(),
            })
        };
        perform(&mut performer, action, &mut prepared)
            .await
            .unwrap();
        journal.append(&Record::Done { seq }).unwrap();
    }
    journal.append(&Record::Committing).unwrap();
    drop(journal);
    drop(performer);

    let mut next = Performer::new(Files::open(root.path(), "r2").unwrap());
    let recovered = recover_all(journals.path(), &mut next, &Scope::root()).await;

    assert!(recovered.failures.is_empty(), "{:?}", recovered.failures);
    let after = listing(root.path());
    assert!(
        after.iter().all(
            |(path, _)| !["mix-backup", "mix-aside", "mix-new", "mix-remove"]
                .iter()
                .any(|kept| path.to_string_lossy().contains(kept))
        ),
        "{after:?}"
    );
    assert!(root.path().join("nix/.mix-managed").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("etc/nix.conf")).unwrap(),
        "trusted-users = root\n"
    );
}

#[tokio::test]
async fn only_a_journal_no_running_request_holds_is_an_interrupted_request() {
    let root = machine();
    let journals = tempfile::tempdir().unwrap();
    let running = FileJournal::create(journals.path(), "r1").unwrap();
    drop(FileJournal::create(journals.path(), "r2").unwrap());

    let found: Vec<String> = abandoned(journals.path())
        .into_iter()
        .map(|request| request.request)
        .collect();
    assert_eq!(found, ["r2"]);

    let mut next = Performer::new(Files::open(root.path(), "r3").unwrap());
    let recovered = recover_all(journals.path(), &mut next, &Scope::root()).await;

    assert_eq!(recovered.requests, 1);
    assert_eq!(
        unfinished(journals.path()),
        [journals.path().join("r1.ndjson")]
    );
    drop(running);
}

#[tokio::test]
async fn a_journal_recovery_could_not_put_back_is_kept_for_the_next_try() {
    let root = machine();
    let journals = tempfile::tempdir().unwrap();
    let mut journal = FileJournal::create(journals.path(), "r1").unwrap();
    journal
        .append(&Record::Prepared {
            seq: 0,
            undo: vec![Action::Restore {
                path: "/etc/nix.conf".into(),
                from: "/etc/.nix.conf.mix-backup-r1-1".into(),
                expect: Expect::Absent,
            }],
        })
        .unwrap();
    journal.append(&Record::Done { seq: 0 }).unwrap();
    drop(journal);

    let mut next = Performer::new(Files::open(root.path(), "r2").unwrap());
    let recovered = recover_all(journals.path(), &mut next, &Scope::root()).await;

    assert_eq!(recovered.failures.len(), 1, "{:?}", recovered.failures);
    assert_eq!(
        abandoned(journals.path()),
        [Abandoned {
            request: "r1".into(),
            pending: vec!["/etc/nix.conf".into()],
        }]
    );
}
