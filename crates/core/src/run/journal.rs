use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::effect::Action;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Record {
    Began { request: String },
    Prepared { seq: u64, undo: Vec<Action> },
    Done { seq: u64 },
    Settled { seq: u64, undo: Vec<Action> },
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

fn confirmed(records: &[Record]) -> Vec<(u64, Vec<Action>)> {
    let mut prepared: Vec<(u64, Vec<Action>)> = Vec::new();
    let mut confirmed = Vec::new();
    for record in records {
        match record {
            Record::Prepared { seq, undo } => {
                prepared.retain(|(earlier, _)| earlier != seq);
                prepared.push((*seq, undo.clone()));
            }
            Record::Done { seq } => {
                if let Some((_, undo)) = prepared.iter().find(|(prepared, _)| prepared == seq) {
                    confirmed.push((*seq, undo.clone()));
                }
            }
            Record::Settled { seq, undo } => confirmed.push((*seq, undo.clone())),
            _ => {}
        }
    }
    confirmed
}

pub fn pending(records: &[Record]) -> Vec<PathBuf> {
    confirmed(records)
        .into_iter()
        .flat_map(|(_, undo)| undo)
        .filter_map(|action| match action {
            Action::Restore { from, .. } => Some(from),
            _ => None,
        })
        .collect()
}

pub fn abandoned(request: &str, records: &[Record]) -> crate::effect::Abandoned {
    let pending = match recover(records) {
        Recovery::RollBack { uncertain, certain } => certain
            .iter()
            .chain(&uncertain)
            .map(|action| action.describe().1)
            .collect(),
        Recovery::FinishCommit { pending } => pending
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        Recovery::Nothing => Vec::new(),
    };
    crate::effect::Abandoned {
        request: request.to_string(),
        pending,
    }
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
    let confirmed = confirmed(records);
    let settled: HashSet<u64> = confirmed.iter().map(|(seq, _)| *seq).collect();
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
    let journal: Vec<Vec<Action>> = confirmed.iter().map(|(_, undo)| still(undo)).collect();
    let mut latest: Vec<(u64, &[Action])> = Vec::new();
    for record in records {
        if let Record::Prepared { seq, undo } = record
            && !settled.contains(seq)
            && !failed.contains(seq)
        {
            match latest.iter_mut().find(|(earlier, _)| earlier == seq) {
                Some(entry) => entry.1 = undo,
                None => latest.push((*seq, undo)),
            }
        }
    }
    let mut uncertain = Vec::new();
    for (_, undo) in latest {
        uncertain.extend(rollback_order(&[still(undo)]));
    }
    let certain = rollback_order(&journal);
    if uncertain.is_empty() && certain.is_empty() {
        return Recovery::Nothing;
    }
    Recovery::RollBack { uncertain, certain }
}

pub fn rollback_order(journal: &[Vec<Action>]) -> Vec<Action> {
    let mut ordered = Vec::new();
    let mut pending: Vec<Action> = Vec::new();
    for undo in journal.iter().rev() {
        for action in undo {
            if action.is_sync() {
                if !pending.contains(action) {
                    pending.push(action.clone());
                }
                continue;
            }
            if action.needs_loaded_units()
                && let Some(index) = pending
                    .iter()
                    .position(|sync| *sync == Action::DaemonReload)
            {
                ordered.push(pending.remove(index));
            }
            ordered.push(action.clone());
        }
    }
    pending.reverse();
    ordered.extend(pending);
    ordered
}

#[cfg(test)]
mod tests {
    fn unit(name: &str) -> String {
        name.to_string()
    }

    fn restore(path: &str) -> Action {
        Action::Restore {
            path: PathBuf::from(path),
            from: PathBuf::from(format!("{path}.mix-backup")),
            expect: Expect::Present(FileId {
                dev: 1,
                ino: 2,
                born: None,
            }),
        }
    }

    #[test]
    fn only_reloads_and_restarts_are_syncs() {
        assert!(Action::DaemonReload.is_sync());
        assert!(
            Action::RestartUnit {
                unit: unit("nix-daemon.service")
            }
            .is_sync()
        );
        assert!(
            !Action::StartUnit {
                unit: unit("nix-daemon.socket")
            }
            .is_sync()
        );
        assert!(!restore("/etc/nix/nix.conf").is_sync());
    }

    #[test]
    fn a_rollback_undoes_in_reverse_and_syncs_once_after_everything_else() {
        let journal = vec![
            vec![
                restore("/etc/systemd/system/nix-daemon.service"),
                Action::DaemonReload,
            ],
            vec![
                restore("/etc/systemd/system/nix-daemon.socket"),
                Action::DaemonReload,
            ],
            vec![Action::DaemonReload],
            vec![Action::DisableUnit {
                unit: unit("nix-daemon.socket"),
            }],
        ];

        assert_eq!(
            rollback_order(&journal),
            [
                Action::DisableUnit {
                    unit: unit("nix-daemon.socket")
                },
                restore("/etc/systemd/system/nix-daemon.socket"),
                restore("/etc/systemd/system/nix-daemon.service"),
                Action::DaemonReload,
            ]
        );
    }

    #[test]
    fn different_syncs_keep_the_order_they_were_first_needed_in() {
        let restart = Action::RestartUnit {
            unit: unit("nix-daemon.service"),
        };
        let journal = vec![
            vec![restore("/etc/nix/nix.conf"), restart.clone()],
            vec![
                restore("/etc/systemd/system/nix-daemon.service"),
                Action::DaemonReload,
            ],
        ];

        let order = rollback_order(&journal);

        assert_eq!(&order[order.len() - 2..], [restart, Action::DaemonReload]);
    }

    #[test]
    fn a_pending_reload_runs_before_a_unit_is_touched_again() {
        let socket = || unit("nix-daemon.socket");
        let journal = vec![
            vec![Action::StartUnit { unit: socket() }],
            vec![Action::EnableUnit { unit: socket() }],
            vec![restore("/etc/systemd/system/nix-daemon.socket")],
            vec![Action::DaemonReload],
            vec![restore("/nix")],
        ];

        assert_eq!(
            rollback_order(&journal),
            [
                restore("/nix"),
                restore("/etc/systemd/system/nix-daemon.socket"),
                Action::DaemonReload,
                Action::EnableUnit { unit: socket() },
                Action::StartUnit { unit: socket() },
            ]
        );
    }

    #[test]
    fn an_empty_journal_undoes_nothing() {
        assert!(rollback_order(&[]).is_empty());
    }

    use super::*;
    use crate::effect::{Expect, FileId};

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
    fn a_later_announcement_for_the_same_action_replaces_the_earlier_one() {
        let restore = Action::Restore {
            path: "/home/alice/state".into(),
            from: "/home/alice/.state.mix-reclaim-r".into(),
            expect: Expect::Absent,
        };
        let records = [
            Record::Began {
                request: "r".into(),
            },
            Record::Prepared {
                seq: 0,
                undo: vec![removal("/home/alice/.state.mix-reclaim-r", 7)],
            },
            Record::Prepared {
                seq: 0,
                undo: vec![restore.clone()],
            },
        ];

        assert_eq!(
            recover(&records),
            Recovery::RollBack {
                uncertain: vec![restore.clone()],
                certain: Vec::new(),
            }
        );
        let done = [records.to_vec(), vec![Record::Done { seq: 0 }]].concat();
        assert_eq!(
            recover(&done),
            Recovery::RollBack {
                uncertain: Vec::new(),
                certain: vec![restore],
            }
        );
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
    fn a_settled_change_is_undone_with_the_undo_it_settled_on() {
        let predicted = removal("/home/alice/.local/state/mix/.git", 0);
        let actual = removal("/home/alice/.local/state/mix/.git", 9);
        let records = [
            Record::Prepared {
                seq: 0,
                undo: vec![predicted],
            },
            Record::Settled {
                seq: 0,
                undo: vec![actual.clone()],
            },
        ];

        assert_eq!(
            recover(&records),
            Recovery::RollBack {
                uncertain: vec![],
                certain: vec![actual],
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
