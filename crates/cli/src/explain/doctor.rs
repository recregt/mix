//! What `mix doctor` says: about a single inspection, and about the audit as a whole.
//!
//! The audit hands over measurements, such as a mode, a pair of ids, or a unit that is not
//! running, and every one of them is written here. The match is exhaustive on purpose: a new
//! inspection cannot reach a terminal without someone deciding how it reads.

use mix_shell::ops::doctor::HealthReport;
use mix_shell::target::Finding;
use mix_ui::{help, phrase, write_phrase};

use super::{Diagnostic, failed};

pub(crate) const COMMAND: &str = "mix doctor";

pub(crate) const ACTION: &str = "finish the health check";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<mix_core::Error>() {
        Some(error) => super::core_error(error, COMMAND, &ACTION),
        None => failed(&ACTION),
    }
}

pub struct Measured(pub Finding);

impl std::fmt::Display for Measured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Finding::Missing => write_phrase!(f, "missing"),
            Finding::Unreadable { kind } => {
                write_phrase!(f, "cannot be read: {}", std::io::Error::from(kind))
            }
            Finding::NotADirectory => write_phrase!(f, "exists but is not a directory"),
            Finding::Mode { actual, expected } => {
                write_phrase!(f, "mode is {actual:o}, expected {expected:o}")
            }
            Finding::Owner {
                actual: (uid, gid),
                expected: (want_uid, want_gid),
            } => write_phrase!(f, "owned by {uid}:{gid}, expected {want_uid}:{want_gid}"),
            Finding::ContentDrift => write_phrase!(f, "was changed outside `mix`"),
            Finding::GroupMissing => write_phrase!(f, "the group does not exist"),
            Finding::GroupGid { actual, expected } => {
                write_phrase!(f, "gid is {actual}, expected {expected}")
            }
            Finding::NotAMember { group } => write_phrase!(f, "not a member of the {group} group"),
            Finding::NoSuchUser => write_phrase!(f, "the user no longer exists"),
            Finding::UserMissing => write_phrase!(f, "the user does not exist"),
            Finding::UserIds {
                actual: (uid, gid),
                expected: (want_uid, want_gid),
            } => write_phrase!(f, "uid/gid is {uid}/{gid}, expected {want_uid}/{want_gid}"),
            Finding::UnitMissing => write_phrase!(f, "unit file missing"),
            Finding::UnitDrift => write_phrase!(f, "unit file was changed"),
            Finding::UnitInactive => write_phrase!(f, "unit is not active"),
            Finding::RuntimeMissing => {
                write_phrase!(f, "missing, and `mix repair` can't restore it")
            }
        }
    }
}

pub fn finding(finding: Finding) -> Measured {
    Measured(finding)
}

pub fn check(report: &HealthReport) -> Diagnostic {
    let Some(found) = report.finding else {
        return Diagnostic::new(phrase!("{}: unhealthy", report.name));
    };
    let words = Diagnostic::new(phrase!("{}: {}", report.name, finding(found)));
    match found.unfixable() {
        Some(reason) => words.help(super::target::unfixable(reason)),
        None => words,
    }
}

pub fn unhealthy(reports: &[HealthReport]) -> Diagnostic {
    let summary = phrase!("some checks failed");
    let mut unfixable = None;
    for report in reports.iter().filter(|report| !report.healthy()) {
        match report.finding.and_then(Finding::unfixable) {
            Some(reason) => unfixable = unfixable.or(Some(reason)),
            None => return Diagnostic::new(summary).help(help!("run `mix repair` to fix them")),
        }
    }

    match unfixable {
        Some(reason) => Diagnostic::new(summary).help(super::target::unfixable(reason)),
        None => Diagnostic::new(summary),
    }
}

#[cfg(test)]
mod tests {
    use mix_core::Category;

    use super::*;

    fn report(name: &str, finding: Option<Finding>) -> HealthReport {
        HealthReport {
            name: name.to_string(),
            category: Category::Filesystem,
            finding,
        }
    }

    #[test]
    fn every_finding_reads_as_a_phrase_with_its_data_in_place() {
        let findings = [
            Finding::Missing,
            Finding::Unreadable {
                kind: std::io::ErrorKind::PermissionDenied,
            },
            Finding::NotADirectory,
            Finding::Mode {
                actual: 0o700,
                expected: 0o755,
            },
            Finding::Owner {
                actual: (1, 1),
                expected: (0, 0),
            },
            Finding::ContentDrift,
            Finding::GroupMissing,
            Finding::GroupGid {
                actual: 1,
                expected: 30000,
            },
            Finding::NotAMember { group: "nixbld" },
            Finding::NoSuchUser,
            Finding::UserMissing,
            Finding::UserIds {
                actual: (1, 1),
                expected: (0, 0),
            },
            Finding::UnitMissing,
            Finding::UnitDrift,
            Finding::UnitInactive,
            Finding::RuntimeMissing,
        ];
        for finding in findings {
            match finding {
                Finding::Missing
                | Finding::Unreadable { .. }
                | Finding::NotADirectory
                | Finding::Mode { .. }
                | Finding::Owner { .. }
                | Finding::ContentDrift
                | Finding::GroupMissing
                | Finding::GroupGid { .. }
                | Finding::NotAMember { .. }
                | Finding::NoSuchUser
                | Finding::UserMissing
                | Finding::UserIds { .. }
                | Finding::UnitMissing
                | Finding::UnitDrift
                | Finding::UnitInactive
                | Finding::RuntimeMissing => {}
            }
            let reports = [report("/nix", Some(finding))];
            for words in [check(&reports[0]), unhealthy(&reports)] {
                for line in words.message().lines() {
                    assert!(mix_ui::text::is_phrase(line), "{finding:?}: {line:?}");
                }
            }
        }
    }

    #[test]
    fn a_failed_check_is_the_measurement_the_audit_made() {
        assert_eq!(
            check(&report(
                "/nix",
                Some(Finding::Mode {
                    actual: 0o700,
                    expected: 0o755
                })
            ))
            .message(),
            "/nix: mode is 700, expected 755"
        );
    }

    #[test]
    fn a_check_with_nothing_to_say_still_says_it_failed() {
        assert_eq!(
            check(&report("nixbld1", None)).message(),
            "nixbld1: unhealthy"
        );
    }

    #[test]
    fn an_identity_drift_names_both_pairs_of_ids() {
        assert_eq!(
            check(&report(
                "nixbld1",
                Some(Finding::UserIds {
                    actual: (1000, 1000),
                    expected: (30_000, 30_000)
                })
            ))
            .message(),
            "nixbld1: uid/gid is 1000/1000, expected 30000/30000"
        );
    }

    #[test]
    fn a_check_repair_cannot_reconcile_is_listed_with_the_way_out() {
        let line = check(&report("/nix", Some(Finding::NotADirectory))).message();

        assert_eq!(
            line,
            "/nix: exists but is not a directory\nremove it, then run `mix repair` again"
        );
    }

    #[test]
    fn a_check_repair_reconciles_is_left_as_one_line() {
        let line = check(&report("/nix", Some(Finding::ContentDrift))).message();

        assert!(!line.contains('\n'));
    }

    #[test]
    fn a_verdict_offers_repair_when_something_can_be_repaired() {
        let reports = [
            report("default profile", Some(Finding::RuntimeMissing)),
            report("/nix/var", Some(Finding::Missing)),
        ];

        assert!(unhealthy(&reports).message().contains("mix repair"));
    }

    /// Nothing here is repair's to fix, so sending the reader to `mix repair` would only have
    /// them read the same list again.
    #[test]
    fn a_verdict_of_nothing_but_unfixable_findings_sends_the_reader_elsewhere() {
        let reports = [
            report("nix-env", Some(Finding::RuntimeMissing)),
            report("default profile", Some(Finding::RuntimeMissing)),
        ];

        let message = unhealthy(&reports).message();

        assert!(message.contains("mix bootstrap"));
        assert!(!message.contains("mix repair"));
    }

    #[test]
    fn a_healthy_report_is_not_read_as_a_finding() {
        let reports = [report("/nix", None)];

        assert_eq!(unhealthy(&reports).message(), "some checks failed");
    }

    /// Every measurement the audit can make has words of its own.
    #[test]
    fn every_finding_is_written_out() {
        for found in [
            Finding::Missing,
            Finding::Unreadable {
                kind: std::io::ErrorKind::PermissionDenied,
            },
            Finding::NotADirectory,
            Finding::Mode {
                actual: 0o700,
                expected: 0o755,
            },
            Finding::Owner {
                actual: (0, 0),
                expected: (1000, 1000),
            },
            Finding::ContentDrift,
            Finding::GroupMissing,
            Finding::GroupGid {
                actual: 1,
                expected: 30_000,
            },
            Finding::NotAMember { group: "mix-users" },
            Finding::NoSuchUser,
            Finding::UserMissing,
            Finding::UserIds {
                actual: (1, 1),
                expected: (30_000, 30_000),
            },
            Finding::UnitMissing,
            Finding::UnitDrift,
            Finding::UnitInactive,
            Finding::RuntimeMissing,
        ] {
            let words = finding(found).to_string();
            assert!(mix_ui::text::is_phrase(&words), "{found:?}: {words}");
        }
    }
}
