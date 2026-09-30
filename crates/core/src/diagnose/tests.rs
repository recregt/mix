use std::io::ErrorKind;

use mix_events::v1::Cancellation;
use mix_events::{Diagnose, Fault};

use super::*;
use crate::action::{UnitFailure, UnitOperation};

fn every_error() -> Vec<Error> {
    let exhaustive = |error: &Error| match error {
        Error::Io { .. }
        | Error::Command { .. }
        | Error::Exec { .. }
        | Error::TaskPanicked(_)
        | Error::Cancelled { .. }
        | Error::Locked { .. }
        | Error::LockMissing { .. } => {}
    };
    let errors = vec![
        Error::Io {
            path: "/nix".into(),
            source: ErrorKind::NotFound.into(),
        },
        Error::Command {
            command: "nix build".into(),
            detail: "error: attribute missing".into(),
        },
        Error::Exec {
            command: "nix".into(),
            source: ErrorKind::NotFound.into(),
        },
        Error::TaskPanicked("boom".into()),
        Error::Cancelled {
            command: "mix install".into(),
        },
        Error::Locked {
            path: "/var/lib/mix/lock".into(),
        },
        Error::LockMissing {
            path: "/var/lib/mix/lock".into(),
        },
    ];
    errors.iter().for_each(exhaustive);
    errors
}

fn every_failure() -> Vec<Failure> {
    let exhaustive = |failure: &Failure| match failure {
        Failure::Conflict { .. }
        | Failure::Io { .. }
        | Failure::CommandFailed { .. }
        | Failure::SpawnFailed { .. }
        | Failure::Unit(_)
        | Failure::SystemdUnreachable
        | Failure::Network { .. }
        | Failure::Integrity { .. }
        | Failure::Cancelled
        | Failure::Unrepairable { .. } => {}
    };
    let failures = vec![
        Failure::Conflict {
            subject: "/etc/nix/nix.conf".into(),
            expected: "the file mix wrote".into(),
            found: "another file".into(),
        },
        Failure::Io {
            path: "/nix".into(),
            kind: ErrorKind::PermissionDenied,
        },
        Failure::CommandFailed {
            program: "nix build".into(),
            status: Some(1),
            output_tail: "error".into(),
        },
        Failure::SpawnFailed {
            program: "nix".into(),
            kind: ErrorKind::NotFound,
        },
        Failure::Unit(Box::new(UnitFailure {
            operation: UnitOperation::Start,
            unit: "nix-daemon.socket".into(),
            job_result: "failed".into(),
            active_state: "failed".into(),
            sub_state: "failed".into(),
            unit_result: "exit-code".into(),
            invocation: None,
        })),
        Failure::SystemdUnreachable,
        Failure::Network {
            url: "https://mirror.internal".into(),
        },
        Failure::Integrity {
            artifact: "nix archive".into(),
            expected: "aa".into(),
            found: "bb".into(),
        },
        Failure::Cancelled,
        Failure::Unrepairable {
            artifact: "/nix/store".into(),
            reason: Unfixable::MissingRuntime,
        },
    ];
    failures.iter().for_each(exhaustive);
    failures
}

#[test]
fn every_core_error_has_a_code_unless_it_is_internal_or_an_interruption() {
    for error in every_error() {
        match (&error, error.fault()) {
            (Error::TaskPanicked(_), fault) => assert_eq!(fault.code(), Some(Code::Internal)),
            (Error::Cancelled { .. }, fault) => {
                assert_eq!(
                    fault,
                    Fault::Cancelled {
                        cause: Cancellation::Interrupted,
                        rolled_back: false,
                    }
                )
            }
            (_, fault) => {
                let code = fault.code().expect("a failure has a code");
                assert!(
                    !matches!(code, Code::Internal | Code::Unspecified),
                    "{error:?}"
                );
            }
        }
    }
}

#[test]
fn every_action_failure_has_a_code_unless_it_is_an_interruption() {
    for failure in every_failure() {
        match (&failure, failure.fault()) {
            (Failure::Cancelled, fault) => {
                assert_eq!(
                    fault,
                    Fault::Cancelled {
                        cause: Cancellation::Interrupted,
                        rolled_back: false,
                    }
                )
            }
            (_, fault) => {
                let code = fault.code().expect("a failure has a code");
                assert!(
                    !matches!(code, Code::Internal | Code::Unspecified),
                    "{failure:?}"
                );
            }
        }
    }
}

#[test]
fn a_denied_permission_is_told_apart_from_other_io() {
    let denied = Error::Io {
        path: "/nix".into(),
        source: ErrorKind::PermissionDenied.into(),
    };

    assert_eq!(denied.fault().code(), Some(Code::PermissionDenied));
}

#[test]
fn an_unrepairable_artifact_carries_its_reason() {
    let Fault::Failed(diagnostic) = Failure::Unrepairable {
        artifact: "/nix/store".into(),
        reason: Unfixable::MissingRuntime,
    }
    .fault() else {
        panic!("an unrepairable artifact is a failure");
    };

    assert_eq!(
        diagnostic.detail,
        Some(Detail::Unrepairable(UnrepairableDetail {
            artifact: "/nix/store".into(),
            reason: WireUnfixable::MissingRuntime as i32,
        }))
    );
}

#[test]
fn a_lock_message_reads_as_the_error_does() {
    use std::os::unix::ffi::OsStrExt;

    let odd = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/var/lib/mix/\xFFlock"));
    for path in [std::path::PathBuf::from("/var/lib/mix/lock"), odd] {
        for error in [
            Error::Locked { path: path.clone() },
            Error::LockMissing { path: path.clone() },
        ] {
            let Fault::Failed(diagnostic) = error.fault() else {
                panic!("a lock error is a failure");
            };
            assert_eq!(diagnostic.message, error.to_string());
            assert_eq!(
                diagnostic.detail,
                Some(Detail::Lock(LockDetail {
                    path: path.display().to_string(),
                    holder: None,
                }))
            );
        }
    }
}

#[test]
fn the_quick_code_is_the_code_of_the_full_diagnostic() {
    for error in every_error() {
        assert_eq!(error.code(), error.fault().code(), "{error:?}");
    }
}
