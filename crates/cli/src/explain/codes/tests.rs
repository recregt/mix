use std::path::PathBuf;

use mix_events::Fault;
use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{
    ConflictDetail, Diagnostic as Wire, FormatDetail, Host, HostDetail, IoDetail, PackagesDetail,
    Severity, TargetDetail, Unfixable, UnitDetail, UnrepairableDetail,
};

use super::*;
use crate::explain::{Context, render, rpc_fault};

fn every_code() -> Vec<Code> {
    (0..=i32::from(u8::MAX))
        .filter_map(|value| Code::try_from(value).ok())
        .filter(|code| *code != Code::Unspecified)
        .collect()
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
    let mut failed = diagnostic(code);
    if code == Code::RollbackIncomplete {
        failed.causes.push(diagnostic(Code::Network));
    }
    Fault::Failed(failed)
}

fn golden(code: Code) -> String {
    let words = render(
        &sample(code),
        &Context {
            command: "mix install",
            action: &"install ripgrep",
        },
    )
    .message();
    format!("{words}\n\n{}\n", long(code))
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/explain")
}

#[test]
fn every_code_has_a_name_that_parses_back_and_a_long_text() {
    for code in every_code() {
        assert_eq!(parse(name(code)), Some(code), "{code:?}");
        assert_eq!(parse(&name(code).to_lowercase()), Some(code), "{code:?}");
        assert_eq!(parse(code.as_str_name()), Some(code), "{code:?}");
        assert!(long(code).len() > 40, "{code:?}");
    }
}

#[test]
fn a_name_mix_does_not_use_is_not_a_code() {
    assert_eq!(parse("NOPE"), None);
    assert_eq!(parse("UNSPECIFIED"), None);
    assert_eq!(parse(""), None);
}

#[test]
fn every_code_reads_as_its_golden_file() {
    let update = std::env::var("MIX_UPDATE_GOLDEN").is_ok_and(|value| value == "1");
    let dir = golden_dir();
    for code in every_code() {
        let path = dir.join(format!("{}.txt", name(code)));
        let rendered = golden(code);
        if update {
            std::fs::write(&path, &rendered).unwrap();
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "{} is missing; run with MIX_UPDATE_GOLDEN=1",
                path.display()
            )
        });
        assert_eq!(rendered, expected, "{}", path.display());
    }
    let recorded = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(recorded, every_code().len(), "a golden file has no code");
}

#[test]
fn every_worker_failure_has_a_code_unless_it_is_a_protocol_violation() {
    use mix_rpc::Error;

    let errors = vec![
        Error::Spawn(std::io::ErrorKind::NotFound.into()),
        Error::Connect("refused".into()),
        Error::Refused("not allowed".into()),
        Error::Ended,
        Error::NotAConnection(std::io::ErrorKind::InvalidInput.into()),
    ];
    for error in &errors {
        let code = rpc_fault(error).code();
        match error {
            Error::Malformed(_) | Error::NotAConnection(_) => {
                assert_eq!(code, Some(Code::Internal))
            }
            Error::Spawn(_)
            | Error::Launch(_)
            | Error::Connect(_)
            | Error::Refused(_)
            | Error::Ended => assert!(
                matches!(code, Some(Code::PrivilegesUnavailable | Code::WorkerEnded)),
                "{error:?}: {code:?}"
            ),
        }
    }
}
