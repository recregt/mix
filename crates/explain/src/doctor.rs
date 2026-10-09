//! What `mix doctor` says: about a single inspection, and about the audit as a whole.
//!
//! The audit hands over measurements, such as a mode, a pair of ids, or a unit that is not
//! running, and every one of them is written here. The match is exhaustive on purpose: a new
//! inspection cannot reach a terminal without someone deciding how it reads, and one this
//! build does not know is still shown as a failed check.

use mix_events::v1::finding::Kind;
use mix_events::v1::{Finding, InspectionReport, Unfixable};
use mix_ui::text::{decimal, octal};
use mix_ui::{Labels, around, help, note, note_around, note_parts, phrase, phrase_parts};

use super::Diagnostic;

pub fn healthy(report: &InspectionReport) -> bool {
    report.finding.is_none()
}

pub fn check(report: &InspectionReport) -> Diagnostic {
    let Some(found) = report
        .finding
        .as_ref()
        .and_then(|finding| finding.kind.as_ref())
    else {
        return Diagnostic::new(around!("", " failed its check").around(&report.target))
            .help(super::report_a_bug());
    };
    let (mut a, mut b, mut c, mut d) = ([0u8; 22], [0u8; 22], [0u8; 22], [0u8; 22]);
    let (summary, note) = match found {
        Kind::Missing(_) => (
            around!("", " is missing"),
            Some(note!("`mix` set it up, and it is no longer there")),
        ),
        Kind::Unreadable(unreadable) => (
            around!("", " can't be read"),
            Some(note!(
                "{}",
                std::io::Error::from(mix_events::io_kind::named(&unreadable.kind))
            )),
        ),
        Kind::NotADirectory(_) => (
            around!("", " is not a directory"),
            Some(note!("something else is in its place")),
        ),
        Kind::Mode(mode) => (
            around!("", " has the wrong permissions"),
            Some(note_parts![
                "its mode is ",
                octal(&mut a, u64::from(mode.actual)),
                ", and `mix` set ",
                octal(&mut b, u64::from(mode.expected)),
                ""
            ]),
        ),
        Kind::Owner(ids) => (
            around!("", " has the wrong owner"),
            Some(note_parts![
                "it is owned by ",
                decimal(&mut a, u64::from(ids.actual_uid)),
                ":",
                decimal(&mut b, u64::from(ids.actual_gid)),
                ", and `mix` set ",
                decimal(&mut c, u64::from(ids.expected_uid)),
                ":",
                decimal(&mut d, u64::from(ids.expected_gid)),
                ""
            ]),
        ),
        Kind::ContentDrift(_) => (
            around!("", " was changed outside `mix`"),
            Some(note!("its contents differ from what `mix` wrote")),
        ),
        Kind::GroupMissing(_) => (
            around!("group ", " does not exist"),
            Some(note!("`mix` creates it for the users that build packages")),
        ),
        Kind::GroupGid(gid) => (
            around!("group ", " has the wrong id"),
            Some(note_parts![
                "its gid is ",
                decimal(&mut a, u64::from(gid.actual)),
                ", and `mix` set ",
                decimal(&mut b, u64::from(gid.expected)),
                ""
            ]),
        ),
        Kind::NotAMember(member) => (
            around!("", " is not in a group `mix` manages"),
            Some(note_parts![
                "`mix` adds every user that builds packages to ",
                &member.group,
                ""
            ]),
        ),
        Kind::NoSuchUser(_) => (around!("user ", " no longer exists"), None),
        Kind::UserMissing(_) => (
            around!("user ", " does not exist"),
            Some(note!("`mix` creates it to build packages")),
        ),
        Kind::UserIds(ids) => (
            around!("user ", " has the wrong ids"),
            Some(note_parts![
                "its uid/gid is ",
                decimal(&mut a, u64::from(ids.actual_uid)),
                "/",
                decimal(&mut b, u64::from(ids.actual_gid)),
                ", and `mix` set ",
                decimal(&mut c, u64::from(ids.expected_uid)),
                "/",
                decimal(&mut d, u64::from(ids.expected_gid)),
                ""
            ]),
        ),
        Kind::UnitMissing(_) => (
            around!("the unit file for ", " is missing"),
            Some(note!("`mix` installs it to run the Nix daemon")),
        ),
        Kind::UnitDrift(_) => (
            around!("the unit file for ", " was changed outside `mix`"),
            Some(note!("its contents differ from what `mix` wrote")),
        ),
        Kind::UnitInactive(_) => (
            around!("", " is not running"),
            Some(note!("systemd reports it as inactive")),
        ),
        Kind::RuntimeMissing(_) => (
            around!("", " is missing"),
            Some(note!("`mix repair` can't restore it")),
        ),
        Kind::RepositoryBroken(_) => (
            around!("the history in ", " is damaged"),
            Some(note!(
                "`git` can't read all of it, and `mix repair` starts a new one from the current config"
            )),
        ),
        Kind::Interrupted(interrupted) => (
            around!("an interrupted request in ", " couldn't be put back"),
            Some(note!(
                "request {} still has to put back {}",
                interrupted.requests.join(", "),
                interrupted.pending.join(", ")
            )),
        ),
        Kind::Leftovers(leftovers) => (
            around!("", " are still there"),
            Some(note!("{}", leftovers.paths.join(", "))),
        ),
        Kind::GenerationDangling(dangling) => (
            around!("a generation in ", " no longer exists"),
            Some(note_parts![
                "generation ",
                decimal(&mut a, dangling.generation),
                " links to a store path that is gone"
            ]),
        ),
        Kind::InTheWay(in_the_way) => (
            around!("files in ", " are in the way of files `mix` manages"),
            Some(note!("{}", in_the_way.paths.join(", "))),
        ),
        Kind::RepositoryLocked(_) => (
            around!("", " is locked"),
            Some(note!(
                "a `git` command left `index.lock` behind, and no `mix` command is using it"
            )),
        ),
    };
    let mut words = Diagnostic::new(summary.around(&report.target));
    if let Some(note) = note {
        words = words.note(note);
    }
    match super::render::unfixable(report.unfixable()) {
        Some(help) => words.help(help),
        None => words,
    }
}

pub fn labels(finding: &Finding) -> Labels {
    match finding.kind {
        Some(Kind::UnitDrift(_)) => Labels {
            added: phrase!("not in the Nix runtime's copy"),
            changed: phrase!("differs from the Nix runtime's copy"),
            missing: note_around!("lines of the Nix runtime's copy are missing at line ", ""),
            fix: phrase!("`mix repair` puts back the Nix runtime's copy"),
        },
        Some(
            Kind::Missing(_)
            | Kind::Unreadable(_)
            | Kind::NotADirectory(_)
            | Kind::Mode(_)
            | Kind::Owner(_)
            | Kind::ContentDrift(_)
            | Kind::GroupMissing(_)
            | Kind::GroupGid(_)
            | Kind::NotAMember(_)
            | Kind::NoSuchUser(_)
            | Kind::UserMissing(_)
            | Kind::UserIds(_)
            | Kind::UnitMissing(_)
            | Kind::UnitInactive(_)
            | Kind::RuntimeMissing(_)
            | Kind::RepositoryBroken(_)
            | Kind::RepositoryLocked(_)
            | Kind::Interrupted(_)
            | Kind::Leftovers(_)
            | Kind::GenerationDangling(_)
            | Kind::InTheWay(_),
        )
        | None => Labels {
            added: phrase!("not written by `mix`"),
            changed: phrase!("differs from what `mix` wrote"),
            missing: note_around!("lines `mix` wrote are missing at line ", ""),
            fix: phrase!("`mix repair` puts back what `mix` wrote"),
        },
    }
}

pub fn unhealthy(reports: &[InspectionReport]) -> Diagnostic {
    let (problems, fixable) = reports.iter().filter(|report| !healthy(report)).fold(
        (0u64, 0u64),
        |(problems, fixable), report| {
            let repairable = report.unfixable() == Unfixable::Unspecified;
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
    use mix_core::ops::health::{Finding, HealthReport, wire};

    use super::*;

    fn report(name: &str, finding: Option<Finding>) -> InspectionReport {
        wire::report(&HealthReport {
            name: name.to_string(),
            category: Category::Filesystem,
            finding,
            drift: None,
        })
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
            Finding::RepositoryBroken,
            Finding::RepositoryLocked,
            Finding::Interrupted {
                requests: vec!["01a1041e-1236-7480-a44a-0892d5aff06a".into()],
                pending: vec!["/etc/nix/nix.conf".into()],
            },
            Finding::Leftovers {
                paths: vec![
                    "/etc/nix/.nix.conf.mix-backup-r1-1".into(),
                    "/home/alice/.local/state/mix/.state.mix-new-r1-2".into(),
                ],
            },
            Finding::GenerationDangling { generation: 3 },
            Finding::InTheWay {
                paths: vec!["/home/alice/.bashrc".into()],
            },
        ];
        for finding in findings {
            match &finding {
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
                | Finding::RuntimeMissing
                | Finding::RepositoryBroken
                | Finding::RepositoryLocked
                | Finding::Interrupted { .. }
                | Finding::Leftovers { .. }
                | Finding::GenerationDangling { .. }
                | Finding::InTheWay { .. } => {}
            }
            let reports = [report("/nix", Some(finding.clone()))];
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
    fn a_finding_this_build_does_not_know_is_shown_and_counted() {
        let unknown = InspectionReport {
            finding: Some(mix_events::v1::Finding { kind: None }),
            ..report("nixbld1", None)
        };

        assert_eq!(
            check(&unknown).message(),
            "nixbld1 failed its check\nreport this bug at https://github.com/recregt/mix/issues"
        );
        assert!(!healthy(&unknown));
        assert!(
            unhealthy(&[unknown, report("/nix", None)])
                .message()
                .starts_with("found 1 problem\n")
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
