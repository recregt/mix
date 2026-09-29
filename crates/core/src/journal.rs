use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::action::{Action, rollback_order};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Record {
    Began { request: String },
    Prepared { seq: u64, undo: Vec<Action> },
    Done { seq: u64 },
    Failed { seq: u64 },
    Reverted { action: Action },
    Committing,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    Nothing,
    RollBack {
        uncertain: Vec<Action>,
        certain: Vec<Action>,
    },
    FinishCommit {
        pending: Vec<PathBuf>,
    },
}

pub fn pending(records: &[Record]) -> Vec<PathBuf> {
    let finished = finished(records);
    records
        .iter()
        .filter_map(|record| match record {
            Record::Prepared { seq, undo } if finished.contains(seq) => Some(undo),
            _ => None,
        })
        .flatten()
        .filter_map(|action| match action {
            Action::Restore { from, .. } => Some(from.clone()),
            _ => None,
        })
        .collect()
}

fn finished(records: &[Record]) -> HashSet<u64> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Done { seq } => Some(*seq),
            _ => None,
        })
        .collect()
}

pub fn recover(records: &[Record]) -> Recovery {
    if records.contains(&Record::Ended) {
        return Recovery::Nothing;
    }
    if records.contains(&Record::Committing) {
        return Recovery::FinishCommit {
            pending: pending(records),
        };
    }
    let done = finished(records);
    let failed: HashSet<u64> = records
        .iter()
        .filter_map(|record| match record {
            Record::Failed { seq } => Some(*seq),
            _ => None,
        })
        .collect();
    let mut reverted: Vec<&Action> = records
        .iter()
        .filter_map(|record| match record {
            Record::Reverted { action } => Some(action),
            _ => None,
        })
        .collect();
    let mut still = |undo: &[Action]| -> Vec<Action> {
        undo.iter()
            .filter(
                |action| match reverted.iter().position(|done| done == action) {
                    Some(index) => {
                        reverted.swap_remove(index);
                        false
                    }
                    None => true,
                },
            )
            .cloned()
            .collect()
    };
    let mut journal = Vec::new();
    let mut uncertain = Vec::new();
    for record in records {
        let Record::Prepared { seq, undo } = record else {
            continue;
        };
        if done.contains(seq) {
            journal.push(still(undo));
        } else if !failed.contains(seq) {
            uncertain.extend(rollback_order(&[still(undo)]));
        }
    }
    let certain = rollback_order(&journal);
    if uncertain.is_empty() && certain.is_empty() {
        return Recovery::Nothing;
    }
    Recovery::RollBack { uncertain, certain }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Expect, FileId};

    fn id(ino: u64) -> FileId {
        FileId {
            dev: 1,
            ino,
            born: None,
        }
    }

    fn removal(path: &str, ino: u64) -> Action {
        Action::RemoveCreated {
            path: path.into(),
            expect: id(ino),
        }
    }

    #[test]
    fn a_finished_request_needs_nothing() {
        let records = [
            Record::Began {
                request: "r".into(),
            },
            Record::Prepared {
                seq: 0,
                undo: vec![removal("/nix", 1)],
            },
            Record::Done { seq: 0 },
            Record::Committing,
            Record::Ended,
        ];

        assert_eq!(recover(&records), Recovery::Nothing);
    }

    #[test]
    fn a_request_cut_off_while_committing_finishes_its_commit() {
        let backup = Action::Restore {
            path: "/etc/nix/nix.conf".into(),
            from: "/etc/nix/.nix.conf.mix-backup-r-1".into(),
            expect: Expect::Present(id(2)),
        };
        let records = [
            Record::Prepared {
                seq: 0,
                undo: vec![backup],
            },
            Record::Done { seq: 0 },
            Record::Committing,
        ];

        assert_eq!(
            recover(&records),
            Recovery::FinishCommit {
                pending: vec!["/etc/nix/.nix.conf.mix-backup-r-1".into()]
            }
        );
    }

    #[test]
    fn a_request_cut_off_mid_change_undoes_the_change_in_doubt_first_then_the_rest() {
        let records = [
            Record::Prepared {
                seq: 0,
                undo: vec![removal("/nix", 1)],
            },
            Record::Done { seq: 0 },
            Record::Prepared {
                seq: 1,
                undo: vec![removal("/nix/var", 2)],
            },
            Record::Done { seq: 1 },
            Record::Prepared {
                seq: 2,
                undo: vec![removal("/nix/.mix-managed", 3)],
            },
        ];

        assert_eq!(
            recover(&records),
            Recovery::RollBack {
                uncertain: vec![removal("/nix/.mix-managed", 3)],
                certain: vec![removal("/nix/var", 2), removal("/nix", 1)],
            }
        );
    }

    #[test]
    fn a_change_that_failed_has_nothing_to_undo() {
        let records = [
            Record::Prepared {
                seq: 0,
                undo: vec![removal("/nix", 1)],
            },
            Record::Failed { seq: 0 },
        ];

        assert_eq!(recover(&records), Recovery::Nothing);
    }

    #[test]
    fn a_rollback_cut_off_resumes_where_it_stopped() {
        let records = [
            Record::Prepared {
                seq: 0,
                undo: vec![removal("/nix", 1)],
            },
            Record::Done { seq: 0 },
            Record::Prepared {
                seq: 1,
                undo: vec![removal("/nix/var", 2)],
            },
            Record::Done { seq: 1 },
            Record::Reverted {
                action: removal("/nix/var", 2),
            },
        ];

        assert_eq!(
            recover(&records),
            Recovery::RollBack {
                uncertain: vec![],
                certain: vec![removal("/nix", 1)],
            }
        );
    }

    #[test]
    fn records_survive_being_written_and_read_back() {
        let records = vec![
            Record::Began {
                request: "r".into(),
            },
            Record::Prepared {
                seq: 0,
                undo: vec![
                    removal("/nix", 1),
                    Action::PutFile {
                        path: "/etc/nix.conf".into(),
                        contents: std::sync::Arc::from(&b"x"[..]),
                        mode: 0o644,
                        owner: Some((0, 0)),
                        expect: Expect::Absent,
                    },
                    Action::DaemonReload,
                ],
            },
            Record::Done { seq: 0 },
            Record::Committing,
            Record::Ended,
        ];

        let lines: Vec<String> = records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect();
        let read: Vec<Record> = lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(read, records);
    }
}
