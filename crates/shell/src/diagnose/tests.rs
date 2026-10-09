use mix_core::ops::health::Unfixable;
use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{Cancellation, Code};
use mix_events::{Diagnose, Fault};

use super::*;

fn locked() -> mix_core::Error {
    mix_core::Error::LockMissing {
        path: "/var/lib/mix/lock".into(),
    }
}

fn every_bootstrap_error() -> Vec<BootstrapError> {
    let exhaustive = |error: &BootstrapError| match error {
        BootstrapError::Core(_)
        | BootstrapError::Network(_)
        | BootstrapError::Integrity { .. }
        | BootstrapError::UnsupportedTarget(_)
        | BootstrapError::InvalidMirror(_)
        | BootstrapError::Conflict { .. }
        | BootstrapError::Target(_)
        | BootstrapError::Decompression(_)
        | BootstrapError::MalformedArchive(_)
        | BootstrapError::NotRoot(_)
        | BootstrapError::UnsupportedHost
        | BootstrapError::UnsupportedKernel
        | BootstrapError::SystemdNotReady { .. }
        | BootstrapError::SystemdUnreachable
        | BootstrapError::Unit { .. }
        | BootstrapError::AlreadyManaged
        | BootstrapError::CrossDeviceStore { .. }
        | BootstrapError::Interrupted => {}
    };
    let errors = vec![
        BootstrapError::Core(locked()),
        BootstrapError::Network("connection reset".into()),
        BootstrapError::Integrity {
            artifact: "nix archive".into(),
            detail: "sha256 mismatch".into(),
        },
        BootstrapError::UnsupportedTarget("armv7l-linux".into()),
        BootstrapError::InvalidMirror("the mirror must be an http or https URL".into()),
        BootstrapError::Conflict {
            subject: "/etc/nix/nix.conf".into(),
            expected: "the file mix saw".into(),
            found: "another file".into(),
        },
        BootstrapError::Target(TargetError::Unrepairable {
            artifact: "/nix/store".into(),
            reason: Unfixable::MissingRuntime,
        }),
        BootstrapError::Decompression("truncated".into()),
        BootstrapError::MalformedArchive("no store".into()),
        BootstrapError::NotRoot("bootstrap the managed environment"),
        BootstrapError::UnsupportedHost,
        BootstrapError::UnsupportedKernel,
        BootstrapError::SystemdNotReady { host: Host::Wsl },
        BootstrapError::SystemdUnreachable,
        BootstrapError::Unit {
            operation: "start".into(),
            unit: "nix-daemon.socket".into(),
            detail: "failed".into(),
            invocation: None,
        },
        BootstrapError::AlreadyManaged,
        BootstrapError::CrossDeviceStore {
            path: "/nix/store/pkg".into(),
        },
        BootstrapError::Interrupted,
    ];
    errors.iter().for_each(exhaustive);
    errors
}

fn every_change_error() -> Vec<ChangeError> {
    let exhaustive = |error: &ChangeError| match error {
        ChangeError::Core(_)
        | ChangeError::InvalidPackage(_)
        | ChangeError::InvalidState(_)
        | ChangeError::NewerState(_)
        | ChangeError::NotRoot
        | ChangeError::NotBootstrapped
        | ChangeError::Unrecovered => {}
    };
    let rejected = mix_nixgen::HomeModule::new(
        "mix-user",
        std::path::Path::new("/home/mix-user"),
        mix_nixgen::StateVersion::new_static("24.05"),
    )
    .unwrap()
    .packages(["not a valid ident"])
    .unwrap_err();
    let errors = vec![
        ChangeError::Core(locked()),
        ChangeError::InvalidPackage(rejected),
        ChangeError::InvalidState(Invalid::Package("rm -rf".into())),
        ChangeError::NewerState(2),
        ChangeError::NotRoot,
        ChangeError::NotBootstrapped,
        ChangeError::Unrecovered,
    ];
    errors.iter().for_each(exhaustive);
    errors
}

fn has_a_code(fault: Fault, what: &str) {
    let code = fault
        .code()
        .unwrap_or_else(|| panic!("{what} is a failure"));
    assert!(
        !matches!(code, Code::Internal | Code::Unspecified),
        "{what}: {code:?}"
    );
}

#[test]
fn every_bootstrap_error_has_a_code_unless_it_is_an_interruption() {
    for error in every_bootstrap_error() {
        match &error {
            BootstrapError::Interrupted => {
                assert_eq!(
                    error.fault(),
                    Fault::Cancelled {
                        cause: Cancellation::Interrupted,
                        rolled_back: true,
                    }
                )
            }
            _ => has_a_code(error.fault(), &format!("{error:?}")),
        }
    }
}

#[test]
fn every_change_error_has_a_code() {
    for error in every_change_error() {
        has_a_code(error.fault(), &format!("{error:?}"));
    }
}

#[test]
fn every_remove_and_target_error_has_a_code() {
    let errors: Vec<Box<dyn Diagnose>> = vec![
        Box::new(RemoveError::Protected(vec!["git".into()])),
        Box::new(RemoveError::Change(ChangeError::NotBootstrapped)),
        Box::new(TargetError::Core(locked())),
        Box::new(TargetError::Unrepairable {
            artifact: "/nix/store".into(),
            reason: Unfixable::MissingRuntime,
        }),
    ];
    for (index, error) in errors.iter().enumerate() {
        has_a_code(error.fault(), &format!("error {index}"));
    }
}

#[test]
fn a_protected_list_names_its_packages() {
    let Fault::Failed(diagnostic) = RemoveError::Protected(vec!["git".into()]).fault() else {
        panic!("a protected package is a failure");
    };
    assert!(matches!(
        diagnostic.detail,
        Some(Detail::Packages(ref detail)) if detail.packages == ["git"]
    ));
}

#[test]
fn the_quick_code_is_the_code_of_the_full_diagnostic() {
    for error in every_bootstrap_error() {
        assert_eq!(error.code(), error.fault().code(), "{error:?}");
    }
    for error in every_change_error() {
        assert_eq!(error.code(), error.fault().code(), "{error:?}");
    }
    let others: Vec<Box<dyn Diagnose>> = vec![
        Box::new(RemoveError::Protected(vec!["git".into()])),
        Box::new(RemoveError::Change(ChangeError::NotBootstrapped)),
        Box::new(TargetError::Core(locked())),
        Box::new(TargetError::Unrepairable {
            artifact: "/nix/store".into(),
            reason: Unfixable::MissingRuntime,
        }),
    ];
    for (index, error) in others.iter().enumerate() {
        assert_eq!(error.code(), error.fault().code(), "error {index}");
    }
}
