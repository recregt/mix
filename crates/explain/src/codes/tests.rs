use mix_events::Fault;
use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{
    ConflictDetail, Diagnostic as Wire, FormatDetail, Host, HostDetail, IoDetail, PackagesDetail,
    ProgramDetail, Severity, TargetDetail, Unfixable, UnitDetail, UnrepairableDetail,
};

use super::*;
use crate::{Context, render};

fn defined() -> impl Iterator<Item = Code> {
    Code::DEFINED
        .iter()
        .filter_map(|value| Code::try_from(*value).ok())
}

fn every_code() -> Vec<Code> {
    defined().collect()
}

fn detail(code: Code) -> Option<Detail> {
    match code {
        Code::PermissionDenied | Code::Io => Some(Detail::Io(IoDetail {
            path: "/home/mix-user/.local/state/mix/state".into(),
            kind: "PermissionDenied".into(),
        })),
        Code::Conflict => Some(Detail::Conflict(Box::new(ConflictDetail {
            subject: "/etc/nix/nix.conf".into(),
            expected: "the file mix wrote".into(),
            found: "another file".into(),
        }))),
        Code::UnitFailed => Some(Detail::Unit(Box::new(UnitDetail {
            operation: "start".into(),
            unit: "nix-daemon.socket".into(),
            invocation: None,
        }))),
        Code::UnsupportedTarget => Some(Detail::Target(TargetDetail {
            arch: "armv7l".into(),
            os: "linux".into(),
        })),
        Code::SystemdNotReady => Some(Detail::Host(HostDetail {
            host: Host::Wsl as i32,
            state: String::new(),
        })),
        Code::InvalidPackage | Code::InvalidState => Some(Detail::Packages(PackagesDetail {
            packages: vec!["rm -rf".into()],
        })),
        Code::ProtectedPackage => Some(Detail::Packages(PackagesDetail {
            packages: vec!["git".into()],
        })),
        Code::NewerState => Some(Detail::Format(FormatDetail { format: 2 })),
        Code::UnsupportedProgram => Some(Detail::Program(ProgramDetail {
            program: "/usr/bin/git".into(),
            found: "2.30.1".into(),
            oldest: "2.34".into(),
        })),
        Code::Unrepairable => Some(Detail::Unrepairable(UnrepairableDetail {
            artifact: "/nix".into(),
            reason: Unfixable::NotADirectory as i32,
        })),
        _ => None,
    }
}

fn sample(code: Code) -> Fault {
    let diagnostic = |code: Code| Wire {
        code: code as i32,
        severity: Severity::Error as i32,
        node: 0,
        message: match code {
            Code::InvalidMirror => "the mirror must be an http or https URL".into(),
            _ => String::new(),
        },
        causes: Vec::new(),
        detail: detail(code),
    };
    Fault::Failed(diagnostic(code))
}

fn context() -> Context<'static> {
    Context {
        command: "mix install",
        action: &"install ripgrep",
    }
}

fn report_of(fault: &Fault) -> String {
    let words = render(fault, &context());
    mix_ui::report_text(mix_ui::Severity::Error, &words.report(), false)
}

fn report(code: Code) -> String {
    report_of(&sample(code))
}

#[test]
fn every_code_says_what_happened_and_why_and_never_offers_an_empty_step() {
    for code in every_code() {
        let Explanation {
            description,
            why,
            fix,
        } = explanation(code);

        assert!(!description.is_empty(), "{code:?} has no description");
        assert!(!why.is_empty(), "{code:?} has no reason");
        assert!(fix.iter().all(|step| !step.is_empty()), "{code:?}");
    }
}

#[test]
fn an_explanation_reads_its_name_then_what_happened_then_why_then_the_steps_in_order() {
    for code in every_code() {
        let Explanation {
            description,
            why,
            fix,
        } = explanation(code);
        let text = explanation_text(code);

        assert_eq!(text.lines().next(), Some(name(code)), "{code:?}");
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{code:?}: {needle:?} is not in:\n{text}"))
        };
        assert!(at(description) < at(why), "{code:?}");
        let steps: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("  - "))
            .collect();
        assert_eq!(steps, fix, "{code:?}");
        if fix.is_empty() {
            assert!(
                !text.contains("To fix it:"),
                "{code:?} offers no step:\n{text}"
            );
        } else {
            assert!(at(why) < at("To fix it:"), "{code:?}");
            assert!(
                at("To fix it:") < at(&format!("  - {}", fix[0])),
                "{code:?}"
            );
        }
    }
}

#[test]
fn the_list_has_one_row_per_code_with_every_description_in_the_same_column() {
    let codes = every_code();

    let list = list_text(codes.iter().copied());

    let mut lines = list.lines();
    assert_eq!(lines.next(), Some("Failure codes:"));
    let rows: Vec<&str> = lines.by_ref().take(codes.len()).collect();
    assert_eq!(rows.len(), codes.len());
    let width = codes.iter().map(|code| kebab(*code).len()).max().unwrap();
    let column = "    ".len() + width + 1;
    for (code, row) in codes.iter().zip(&rows) {
        let description = explanation(*code).description;
        let description = description.strip_suffix('.').unwrap_or(description);
        assert!(row.starts_with(&format!("    {}", kebab(*code))), "{row}");
        assert_eq!(&row[column..], description, "{row}");
    }
    assert!(
        list.lines()
            .last()
            .is_some_and(|line| line.contains("mix explain <code>")),
        "{list}"
    );
}

#[test]
fn every_failure_is_reported_as_an_error_that_is_not_empty() {
    for code in every_code() {
        let shown = report(code);

        let first = shown.lines().next().unwrap_or_default();
        assert!(
            first
                .strip_prefix("error: ")
                .is_some_and(|rest| !rest.is_empty()),
            "{code:?}: {shown}"
        );
    }
}

#[test]
fn the_details_a_failure_carries_reach_the_words_the_user_reads() {
    for (code, values) in [
        (
            Code::PermissionDenied,
            &["/home/mix-user/.local/state/mix/state"][..],
        ),
        (Code::Conflict, &["/etc/nix/nix.conf"]),
        (Code::UnitFailed, &["nix-daemon.socket"]),
        (Code::UnsupportedTarget, &["armv7l"]),
        (Code::InvalidPackage, &["rm -rf"]),
        (Code::InvalidState, &["rm -rf"]),
        (Code::ProtectedPackage, &["git"]),
        (Code::UnsupportedProgram, &["2.30.1", "2.34"]),
        (Code::Unrepairable, &["/nix"]),
    ] {
        let shown = report(code);

        for value in values {
            assert!(shown.contains(value), "{code:?} hides {value:?}:\n{shown}");
        }
    }
}

#[test]
fn the_advice_for_a_missing_systemd_depends_on_where_mix_runs() {
    let on = |host: Host| {
        report_of(&Fault::Failed(Wire {
            code: Code::SystemdNotReady as i32,
            severity: Severity::Error as i32,
            detail: Some(Detail::Host(HostDetail {
                host: host as i32,
                state: String::new(),
            })),
            ..Wire::default()
        }))
    };

    assert!(on(Host::Wsl).contains("/etc/wsl.conf"));
    assert!(!on(Host::Native).contains("wsl.conf"));
    assert_ne!(on(Host::Wsl), on(Host::Native));
}

#[test]
fn nothing_mix_says_by_default_names_nix_mechanics() {
    let context = crate::render::Context {
        command: "mix install ripgrep",
        action: &"install ripgrep",
    };
    for code in every_code() {
        let wire = mix_events::v1::Diagnostic {
            code: code as i32,
            message: "it failed".into(),
            detail: Some(mix_events::v1::diagnostic::Detail::Packages(
                mix_events::v1::PackagesDetail {
                    packages: vec!["ripgrep".into()],
                },
            )),
            ..mix_events::v1::Diagnostic::default()
        };
        let words = crate::render::render(&mix_events::Fault::Failed(wire), &context).message();
        for text in [words.as_str(), explanation_text(code).as_str()] {
            assert_eq!(
                mix_core::report::vocabulary::nix_mechanics_in(text),
                Vec::<&str>::new(),
                "{code:?}: {text}"
            );
        }
    }
}
