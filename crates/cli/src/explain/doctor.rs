//! What `mix doctor` says: about a single inspection, and about the audit as a whole.
//!
//! The audit hands over measurements, such as a mode, a pair of ids, or a unit that is not
//! running, and every one of them is written here. The match is exhaustive on purpose: a new
//! inspection cannot reach a terminal without someone deciding how it reads.

use mix_shell::ops::doctor::HealthReport;
use mix_shell::target::Finding;
use mix_ui::text::{decimal, octal};
use mix_ui::{Labels, around, help, note, note_around, note_parts, phrase, phrase_parts};

use super::{Diagnostic, failed};

pub(crate) const COMMAND: &str = "mix doctor";

pub(crate) const ACTION: &str = "finish the health check";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<mix_core::Error>() {
        Some(error) => super::core_error(error, COMMAND, &ACTION),
        None => failed(&ACTION),
    }
}

pub fn check(report: &HealthReport) -> Diagnostic {
    let Some(found) = report.finding else {
        return Diagnostic::new(around!("", " failed its check").around(&report.name));
    };
    let (mut a, mut b, mut c, mut d) = ([0u8; 22], [0u8; 22], [0u8; 22], [0u8; 22]);
    let (summary, note) = match found {
        Finding::Missing => (
            around!("", " is missing"),
            Some(note!("`mix` set it up, and it is no longer there")),
        ),
        Finding::Unreadable { kind } => (
            around!("", " can't be read"),
            Some(note!("{}", std::io::Error::from(kind))),
        ),
        Finding::NotADirectory => (
            around!("", " is not a directory"),
            Some(note!("something else is in its place")),
        ),
        Finding::Mode { actual, expected } => (
            around!("", " has the wrong permissions"),
            Some(note_parts![
                "its mode is ",
                octal(&mut a, u64::from(actual)),
                ", and `mix` set ",
                octal(&mut b, u64::from(expected)),
                ""
            ]),
        ),
        Finding::Owner {
            actual: (uid, gid),
            expected: (want_uid, want_gid),
        } => (
            around!("", " has the wrong owner"),
            Some(note_parts![
                "it is owned by ",
                decimal(&mut a, u64::from(uid)),
                ":",
                decimal(&mut b, u64::from(gid)),
                ", and `mix` set ",
                decimal(&mut c, u64::from(want_uid)),
                ":",
                decimal(&mut d, u64::from(want_gid)),
                ""
            ]),
        ),
        Finding::ContentDrift => (
            around!("", " was changed outside `mix`"),
            Some(note!("its contents differ from what `mix` wrote")),
        ),
        Finding::GroupMissing => (
            around!("group ", " does not exist"),
            Some(note!("`mix` creates it for the users that build packages")),
        ),
        Finding::GroupGid { actual, expected } => (
            around!("group ", " has the wrong id"),
            Some(note_parts![
                "its gid is ",
                decimal(&mut a, u64::from(actual)),
                ", and `mix` set ",
                decimal(&mut b, u64::from(expected)),
                ""
            ]),
        ),
        Finding::NotAMember { group } => (
            around!("", " is not in a group `mix` manages"),
            Some(note_parts![
                "`mix` adds every user that builds packages to ",
                group,
                ""
            ]),
        ),
        Finding::NoSuchUser => (around!("user ", " no longer exists"), None),
        Finding::UserMissing => (
            around!("user ", " does not exist"),
            Some(note!("`mix` creates it to build packages")),
        ),
        Finding::UserIds {
            actual: (uid, gid),
            expected: (want_uid, want_gid),
        } => (
            around!("user ", " has the wrong ids"),
            Some(note_parts![
                "its uid/gid is ",
                decimal(&mut a, u64::from(uid)),
                "/",
                decimal(&mut b, u64::from(gid)),
                ", and `mix` set ",
                decimal(&mut c, u64::from(want_uid)),
                "/",
                decimal(&mut d, u64::from(want_gid)),
                ""
            ]),
        ),
        Finding::UnitMissing => (
            around!("the unit file for ", " is missing"),
            Some(note!("`mix` installs it to run the Nix daemon")),
        ),
        Finding::UnitDrift => (
            around!("the unit file for ", " was changed outside `mix`"),
            Some(note!("its contents differ from what `mix` wrote")),
        ),
        Finding::UnitInactive => (
            around!("", " is not running"),
            Some(note!("systemd reports it as inactive")),
        ),
        Finding::RuntimeMissing => (
            around!("", " is missing"),
            Some(note!("`mix repair` can't restore it")),
        ),
    };
    let mut words = Diagnostic::new(summary.around(&report.name));
    if let Some(note) = note {
        words = words.note(note);
    }
    match found.unfixable() {
        Some(reason) => words.help(super::target::unfixable(reason)),
        None => words,
    }
}

pub fn labels(finding: Finding) -> Labels {
    match finding {
        Finding::UnitDrift => Labels {
            added: phrase!("not in the Nix runtime's copy"),
            changed: phrase!("differs from the Nix runtime's copy"),
            missing: note_around!("lines of the Nix runtime's copy are missing at line ", ""),
            fix: phrase!("`mix repair` puts back the Nix runtime's copy"),
        },
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
        | Finding::UnitInactive
        | Finding::RuntimeMissing => Labels {
            added: phrase!("not written by `mix`"),
            changed: phrase!("differs from what `mix` wrote"),
            missing: note_around!("lines `mix` wrote are missing at line ", ""),
            fix: phrase!("`mix repair` puts back what `mix` wrote"),
        },
    }
}

pub fn unhealthy(reports: &[HealthReport]) -> Diagnostic {
    let (problems, fixable) = reports.iter().filter(|report| !report.healthy()).fold(
        (0u64, 0u64),
        |(problems, fixable), report| {
            let repairable = report.finding.and_then(Finding::unfixable).is_none();
            (problems + 1, fixable + u64::from(repairable))
        },
    );
    let mut digits = [0u8; 22];
    let summary = match problems {
        1 => phrase!("found 1 problem"),
        count => phrase_parts![
            "found ",
            mix_ui::text::decimal(&mut digits, count),
            " problems"
        ],
    };
    let words = Diagnostic::new(summary);
    match (fixable, problems) {
        (0, _) => words,
        (1, 1) => words.help(help!("run `mix repair` to fix it")),
        (fixed, all) if fixed == all => words.help(help!("run `mix repair` to fix them")),
        _ => words.help(help!("run `mix repair` to fix the ones it can")),
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
            drift: None,
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
    fn a_problem_names_the_item_and_notes_what_was_found_against_what_mix_set() {
        assert_eq!(
            check(&report(
                "/nix",
                Some(Finding::Mode {
                    actual: 0o700,
                    expected: 0o755
                })
            ))
            .message(),
            "/nix has the wrong permissions\nits mode is 700, and `mix` set 755"
        );
        assert_eq!(
            check(&report(
                "nixbld1",
                Some(Finding::UserIds {
                    actual: (1000, 1000),
                    expected: (30_000, 30_000)
                })
            ))
            .message(),
            "user nixbld1 has the wrong ids\nits uid/gid is 1000/1000, and `mix` set 30000/30000"
        );
    }

    #[test]
    fn a_check_with_nothing_to_say_still_says_it_failed() {
        assert_eq!(
            check(&report("nixbld1", None)).message(),
            "nixbld1 failed its check"
        );
    }

    #[test]
    fn a_problem_repair_cannot_fix_carries_its_own_way_out() {
        let words = check(&report("/nix", Some(Finding::NotADirectory))).message();

        assert_eq!(
            words,
            "/nix is not a directory\nsomething else is in its place\nremove it, then run `mix repair` again"
        );
    }

    #[test]
    fn a_problem_repair_fixes_leaves_the_way_out_to_the_verdict() {
        let words = check(&report("/nix", Some(Finding::ContentDrift)));

        assert!(
            !words.message().contains("mix repair"),
            "{}",
            words.message()
        );
    }

    #[test]
    fn a_verdict_offers_repair_for_the_problems_it_can_fix() {
        let one = [report("/nix/var", Some(Finding::Missing))];
        let mixed = [
            report("default profile", Some(Finding::RuntimeMissing)),
            report("/nix/var", Some(Finding::Missing)),
        ];

        assert_eq!(
            unhealthy(&one).message(),
            "found 1 problem\nrun `mix repair` to fix it"
        );
        assert_eq!(
            unhealthy(&mixed).message(),
            "found 2 problems\nrun `mix repair` to fix the ones it can"
        );
    }

    #[test]
    fn a_verdict_of_nothing_repair_can_fix_offers_no_repair() {
        let reports = [
            report("nix-env", Some(Finding::RuntimeMissing)),
            report("default profile", Some(Finding::RuntimeMissing)),
        ];

        assert_eq!(unhealthy(&reports).message(), "found 2 problems");
    }

    #[test]
    fn a_healthy_item_is_not_counted_as_a_problem() {
        let reports = [
            report("/nix", None),
            report("/nix/var", Some(Finding::Missing)),
        ];

        assert!(
            unhealthy(&reports)
                .message()
                .starts_with("found 1 problem\n")
        );
    }
}
