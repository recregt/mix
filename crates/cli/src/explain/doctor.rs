//! What `mix doctor` says: about a single inspection, and about the audit as a whole.
//!
//! The audit hands over measurements — a mode, a pair of ids, a unit that is not running — and
//! every one of them is written here. The match is exhaustive on purpose: a new inspection
//! cannot reach a terminal without someone deciding how it reads.

use std::borrow::Cow;

use mix_app::doctor::HealthReport;
use mix_app::target::Finding;

use super::{Diagnostic, failed};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix doctor";

const ACTION: &str = "finish the health check";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<mix_core::Error>() {
        Some(error) => super::core_error(error, COMMAND, &ACTION),
        None => failed(&ACTION),
    }
}

/// What a single measurement says, in the words the reader gets.
///
/// Only what was found: the artifact is named by the line this is written into, and what to do
/// about it is the finding's remedy rather than part of the measurement.
pub fn finding(finding: Finding) -> Cow<'static, str> {
    match finding {
        Finding::Missing => "missing".into(),
        Finding::Unreadable { kind } => {
            format!("cannot be read: {}", std::io::Error::from(kind)).into()
        }
        Finding::NotADirectory => "exists but is not a directory".into(),
        Finding::Mode { actual, expected } => {
            format!("mode is {actual:o}, expected {expected:o}").into()
        }
        Finding::Owner {
            actual: (uid, gid),
            expected: (want_uid, want_gid),
        } => format!("owned by {uid}:{gid}, expected {want_uid}:{want_gid}").into(),
        Finding::ContentDrift => "was changed outside `mix`".into(),
        Finding::GroupMissing => "the group does not exist".into(),
        Finding::GroupGid { actual, expected } => {
            format!("gid is {actual}, expected {expected}").into()
        }
        Finding::NotAMember { group } => format!("not a member of the {group} group").into(),
        Finding::NoSuchUser => "the user no longer exists".into(),
        Finding::UserMissing => "the user does not exist".into(),
        Finding::UserIds {
            actual: (uid, gid),
            expected: (want_uid, want_gid),
        } => format!("uid/gid is {uid}/{gid}, expected {want_uid}/{want_gid}").into(),
        Finding::UnitMissing => "unit file missing".into(),
        Finding::UnitDrift => "unit file was changed".into(),
        Finding::UnitInactive => "unit is not active".into(),
        Finding::RuntimeMissing => "missing, and `mix repair` can't restore it".into(),
    }
}

/// One line per check: the artifact, and what was measured about it.
///
/// Only the name keeps its own spelling; nothing is added to it.
fn measured(report: &HealthReport) -> String {
    match report.finding {
        Some(found) => {
            let words = finding(found);
            let mut line = String::with_capacity(report.name.len() + words.len() + 2);
            line.push_str(&report.name);
            line.push_str(": ");
            line.push_str(&words);
            line
        }
        None => format!("{}: unhealthy", report.name),
    }
}

/// What `mix doctor` says about a failed check, for printing under the artifact's own name.
///
/// A check `mix repair` will reconcile is one line; one it will not is that line with the way
/// out under it, in the same words `mix repair` itself would have used — the finding is bound to
/// the reason repair cannot touch it, so the two commands cannot drift apart.
pub fn check(report: &HealthReport) -> Cow<'static, str> {
    let Some(found) = report.finding else {
        return "unhealthy".into();
    };
    let words = finding(found);
    match found.unfixable() {
        Some(reason) => format!("{words}\n{}", super::target::unfixable(reason)).into(),
        None => words,
    }
}

/// The verdict at the end of an audit that found something.
///
/// Whether `mix repair` is worth suggesting is not a guess: it is worth suggesting when at least
/// one of the findings is one repair reconciles. When none of them are, the reader is sent to
/// the way out of the first one that is in the way instead of being told to run a command that
/// would report the same thing back.
pub fn unhealthy(reports: &[HealthReport]) -> Diagnostic {
    let mut unfixable = None;
    for report in reports.iter().filter(|report| !report.healthy()) {
        match report.finding.and_then(Finding::unfixable) {
            Some(reason) => unfixable = unfixable.or(Some(reason)),
            None => {
                return Diagnostic::hinting("some checks failed", "Run `mix repair` to fix them");
            }
        }
    }

    match unfixable {
        Some(reason) => Diagnostic::hinting("some checks failed", super::target::unfixable(reason)),
        None => Diagnostic::new("some checks failed"),
    }
}

/// The verdict when another command refuses to run on an unhealthy system.
///
/// The reader did not ask for an audit, so the check that failed is named where `mix doctor`
/// would have printed it, and they are told where to look — unless the finding has a way out of
/// its own, which is more use than being sent to a command that cannot fix it.
pub fn blocked(report: &HealthReport) -> Diagnostic {
    let summary = format!("`mix` found a problem: {}", measured(report));
    match report.finding.and_then(Finding::unfixable) {
        Some(reason) => Diagnostic::hinting(summary, super::target::unfixable(reason)),
        None => Diagnostic::hinting(summary, "Run `mix repair` to fix it"),
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
    fn a_failed_check_is_the_measurement_the_audit_made() {
        assert_eq!(
            check(&report(
                "/nix",
                Some(Finding::Mode {
                    actual: 0o700,
                    expected: 0o755
                })
            )),
            "mode is 700, expected 755"
        );
    }

    #[test]
    fn a_check_with_nothing_to_say_still_says_it_failed() {
        assert_eq!(check(&report("nixbld1", None)), "unhealthy");
    }

    /// Both sides of a drift are printed: what is there, and what should be.
    #[test]
    fn an_identity_drift_names_both_pairs_of_ids() {
        assert_eq!(
            check(&report(
                "nixbld1",
                Some(Finding::UserIds {
                    actual: (1000, 1000),
                    expected: (30_000, 30_000)
                })
            )),
            "uid/gid is 1000/1000, expected 30000/30000"
        );
    }

    /// The binding: a check repair cannot reconcile carries repair's own way out.
    #[test]
    fn a_check_repair_cannot_reconcile_is_listed_with_the_way_out() {
        let line = check(&report("/nix", Some(Finding::NotADirectory)));

        assert_eq!(
            line,
            "exists but is not a directory\nRemove it, then run `mix repair` again"
        );
    }

    #[test]
    fn a_check_repair_reconciles_is_left_as_one_line() {
        let line = check(&report("/nix", Some(Finding::ContentDrift)));

        assert!(!line.contains('\n'));
    }

    #[test]
    fn a_blocked_command_names_the_check_and_where_to_look() {
        let message = blocked(&report(
            "/nix",
            Some(Finding::Mode {
                actual: 0o700,
                expected: 0o755,
            }),
        ))
        .message();

        assert!(message.contains("/nix: mode is 700, expected 755"));
        assert!(message.starts_with("`mix` found a problem"));
        assert!(message.contains("mix repair"));
    }

    #[test]
    fn a_blocked_command_sends_a_missing_runtime_to_bootstrap_instead() {
        let message = blocked(&report("default profile", Some(Finding::RuntimeMissing))).message();

        assert!(message.contains("default profile: missing"));
        assert!(message.contains("mix bootstrap"));
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

        assert_eq!(unhealthy(&reports), Diagnostic::new("some checks failed"));
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
            let words = finding(found);
            assert!(!words.is_empty(), "{found:?}");
            assert!(
                !words.ends_with('.'),
                "a measurement is not a sentence: {words}"
            );
        }
    }
}
