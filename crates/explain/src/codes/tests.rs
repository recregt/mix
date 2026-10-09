use insta::assert_snapshot;
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

fn shown(code: Code) -> String {
    let words = render(
        &sample(code),
        &Context {
            command: "mix install",
            action: &"install ripgrep",
        },
    );
    let shown = mix_ui::report_text(mix_ui::Severity::Error, &words.report(), false);
    format!(
        "$ mix install ripgrep\n{shown}\n\n$ mix explain {}\n{}\n",
        kebab(code),
        explanation_text(code)
    )
}

#[test]
fn every_code_has_a_long_text() {
    for code in every_code() {
        assert!(explanation(code).why.len() > 20, "{code:?}");
    }
}

#[test]
fn the_list_reads_as_recorded() {
    assert_snapshot!(
        "explain-list",
        format!("$ mix explain --list\n{}\n", list_text(defined()))
    );
}

#[test]
fn every_code_reads_as_recorded() {
    for code in every_code() {
        assert_snapshot!(name(code), shown(code));
    }
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
                mix_core::vocabulary::nix_mechanics_in(text),
                Vec::<&str>::new(),
                "{code:?}: {text}"
            );
        }
    }
}
