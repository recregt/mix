use std::io::ErrorKind;

use mix_events::v1::Cancellation;
use mix_events::{Diagnose, Fault};

use super::*;
use crate::effect::{CollisionFailure, UnitFailure, UnitOperation};

fn every_error() -> Vec<Error> {
    let exhaustive = |error: &Error| match error {
        Error::Io { .. }
        | Error::Command { .. }
        | Error::Exec { .. }
        | Error::Cancelled { .. }
        | Error::LockMissing { .. }
        | Error::Effect(_) => {}
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
        Error::Cancelled {
            command: "mix install".into(),
        },
        Error::LockMissing {
            path: "/var/lib/mix/lock".into(),
        },
        Error::Effect(Box::new(Failure::CommandFailed {
            program: "git hash-object -- state".into(),
            status: Some(128),
            output_tail: "fatal: could not open 'state' for reading: Permission denied".into(),
        })),
    ];
    errors.iter().for_each(exhaustive);
    errors
}

fn every_failure() -> Vec<Failure> {
    let exhaustive = |failure: &Failure| match failure {
        Failure::Conflict { .. }
        | Failure::Io { .. }
        | Failure::CommandFailed { .. }
        | Failure::NoInodes { .. }
        | Failure::OutOfMemory { .. }
        | Failure::Collision(_)
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
        Failure::NoInodes {
            program: "nix build".into(),
            status: Some(1),
            output_tail: "error: write of 64 bytes: No space left on device".into(),
        },
        Failure::OutOfMemory {
            program: "nix build".into(),
            status: None,
            output_tail: String::new(),
        },
        Failure::Collision(Box::new(CollisionFailure {
            program: "nix build".into(),
            status: Some(1),
            output_tail: "error: two given paths contain a conflicting subpath:\n  `/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-lua-5.4.7/bin/lua' and\n  `/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-lua-5.3.6/bin/lua'".into(),
            file: "bin/lua".into(),
            packages: ["lua5_4".into(), "lua5_3".into()],
        })),
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
fn a_conflict_names_what_changed_and_keeps_what_was_expected_and_found() {
    let conflict = Failure::Conflict {
        subject: "/etc/nix/nix.conf".into(),
        expected: "the file mix wrote".into(),
        found: "another file".into(),
    };

    let diagnostic = diagnostic(&conflict);

    assert_eq!(diagnostic.code(), Code::Conflict);
    assert_eq!(
        diagnostic.message,
        "/etc/nix/nix.conf: expected the file mix wrote, found another file"
    );
    assert_eq!(
        diagnostic.detail,
        Some(Detail::Conflict(Box::new(mix_events::v1::ConflictDetail {
            subject: "/etc/nix/nix.conf".into(),
            expected: "the file mix wrote".into(),
            found: "another file".into(),
        })))
    );
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
            reason: mix_events::v1::Unfixable::MissingRuntime as i32,
        }))
    );
}

#[test]
fn a_lock_message_reads_as_the_error_does() {
    use std::os::unix::ffi::OsStrExt;

    let odd = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/var/lib/mix/\xFFlock"));
    for path in [std::path::PathBuf::from("/var/lib/mix/lock"), odd] {
        let error = Error::LockMissing { path: path.clone() };
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

#[test]
fn the_quick_code_is_the_code_of_the_full_diagnostic() {
    for error in every_error() {
        assert_eq!(error.code(), error.fault().code(), "{error:?}");
    }
}

#[test]
fn a_warning_names_what_could_not_be_done_and_keeps_why() {
    let cause = Failure::Io {
        path: "/var/lib/mix/journal/r1".into(),
        kind: std::io::ErrorKind::PermissionDenied,
    };

    let warning = super::warning(
        Code::JournalUnwritable,
        "could not record progress in the journal",
        &cause,
    );

    assert_eq!(warning.code(), Code::JournalUnwritable);
    assert_eq!(warning.severity(), Severity::Warning);
    assert_eq!(warning.causes.len(), 1);
    assert_eq!(warning.causes[0], diagnostic(&cause));
}

#[test]
fn a_failed_build_names_its_package_and_keeps_nixs_message_as_the_cause() {
    let error = Error::Command {
        command: "/nix/var/nix/profiles/default/bin/nix build".into(),
        detail: "copying...\nerror: Cannot build '/nix/store/bbx79xgf89bvd25i1sivdcykhy39bz14-hello-2.12.drv'.\n       Reason: builder failed with exit code 1.".into(),
    };

    assert_eq!(error.code(), Some(Code::BuildFailed));
    let Fault::Failed(diagnostic) = error.fault() else {
        panic!("a failed command is a failure");
    };
    assert_eq!(diagnostic.code(), Code::BuildFailed);
    assert_eq!(diagnostic.message, "couldn't build hello-2.12");
    assert_eq!(
        diagnostic.detail,
        Some(Detail::Packages(mix_events::v1::PackagesDetail {
            packages: vec!["hello-2.12".into()]
        }))
    );
    let Some(Detail::Command(command)) = &diagnostic.causes[0].detail else {
        panic!("the command is the cause");
    };
    assert!(command.output_tail.starts_with("error: Cannot build"));
    assert!(!command.output_tail.contains("copying"));
}

#[test]
fn an_unknown_package_is_named_in_the_message_and_the_command_is_the_cause() {
    let error = Error::Command {
        command: "/nix/var/nix/profiles/default/bin/nix build".into(),
        detail: "error: attribute 'no-such-package-xyz' missing\n       at /home/alice/.local/state/mix/home.nix:13:7:".into(),
    };

    let Fault::Failed(diagnostic) = error.fault() else {
        panic!("a failed command is a failure");
    };
    assert_eq!(diagnostic.code(), Code::UnknownPackage);
    assert_eq!(
        diagnostic.message,
        "`no-such-package-xyz` isn't a package `mix` can find"
    );
    assert_eq!(
        diagnostic.detail,
        Some(Detail::Packages(mix_events::v1::PackagesDetail {
            packages: vec!["no-such-package-xyz".into()]
        }))
    );
    assert!(matches!(
        &diagnostic.causes[0].detail,
        Some(Detail::Command(command)) if command.command.ends_with("nix build")
    ));
}

const COLLIDED: &str = "error: Cannot build '/nix/store/aaa-home-manager-path.drv'.\n       Reason: builder failed with exit code 25.\n       Last 6 log lines:\n       > pkgs.buildEnv error: two given paths contain a conflicting subpath:\n       >   `/nix/store/9l8xf80ah70hycq9nd24vlri2rzinzcp-lua-5.4.7/include/luaconf.h' and\n       >   `/nix/store/k7qqryx7cwwyb6gpck0bh56izx9g6bpa-lua-5.3.6/include/luaconf.h'\nerror: Cannot build '/nix/store/bbb-home-manager-generation.drv'.\n       Reason: 1 dependency failed.";

#[test]
fn a_collision_names_both_packages_and_the_file_and_keeps_nix_s_words() {
    let failure = Failure::Collision(Box::new(CollisionFailure {
        program: "nix build".into(),
        status: None,
        output_tail: COLLIDED.into(),
        file: "include/luaconf.h".into(),
        packages: ["lua5_4".into(), "lua5_3".into()],
    }));

    let found = diagnostic(&failure);

    assert_eq!(found.code(), Code::PackageCollision);
    assert_eq!(
        found.message,
        "`lua5_4` and `lua5_3` both provide include/luaconf.h"
    );
    assert_eq!(
        found.detail,
        Some(Detail::Packages(PackagesDetail {
            packages: vec!["lua5_4".into(), "lua5_3".into()]
        }))
    );
    let Some(Detail::Command(command)) = &found.causes[0].detail else {
        panic!("the command is the cause");
    };
    assert!(command.output_tail.contains("conflicting subpath"));
    assert!(!command.output_tail.contains("1 dependency failed"));
}

#[test]
fn a_collision_nobody_named_names_the_packages_by_their_store_names() {
    let error = Error::Command {
        command: "nix build".into(),
        detail: COLLIDED.into(),
    };

    let Fault::Failed(found) = error.fault() else {
        panic!("a failed command is a failure");
    };
    assert_eq!(found.code(), Code::PackageCollision);
    assert_eq!(
        found.message,
        "`lua-5.4.7` and `lua-5.3.6` both provide include/luaconf.h"
    );
}

#[test]
fn a_build_stopped_by_running_out_of_inodes_says_so_and_keeps_nix_s_error() {
    let failure = Failure::NoInodes {
        program: "/nix/var/nix/profiles/default/bin/nix build".into(),
        status: None,
        output_tail: "error: creating file \"/nix/store/aaa-gcc-15.3.0/lib/liblsan.a\": No space left on device".into(),
    };

    let found = diagnostic(&failure);

    assert_eq!(found.code(), Code::NoInodes);
    assert_eq!(
        found.message,
        "the disk that holds the Nix store has no free inodes"
    );
    let Some(Detail::Command(command)) = &found.causes[0].detail else {
        panic!("the command is the cause");
    };
    assert!(command.output_tail.contains("No space left on device"));
}

#[test]
fn a_build_killed_for_memory_says_so_and_keeps_the_command() {
    let failure = Failure::OutOfMemory {
        program: "/nix/var/nix/profiles/default/bin/nix build".into(),
        status: None,
        output_tail: "evaluating derivation 'homeConfigurations.alice.activationPackage'".into(),
    };

    let found = diagnostic(&failure);

    assert_eq!(found.code(), Code::OutOfMemory);
    assert_eq!(found.message, "the build ran out of memory");
    let Some(Detail::Command(command)) = &found.detail else {
        panic!("the command is the detail");
    };
    assert!(command.output_tail.contains("activationPackage"));
}

#[test]
fn a_build_stopped_by_a_full_disk_says_so_and_keeps_the_error_that_named_the_disk() {
    let error = Error::Command {
        command: "/nix/var/nix/profiles/default/bin/nix build".into(),
        detail: "error: Cannot build '/nix/store/aaa-home-manager-path.drv'.\n       Reason: builder failed with exit code 1.\n       note: build failure may have been caused by lack of free disk space\nerror: Cannot build '/nix/store/bbb-home-manager-generation.drv'.\n       Reason: 1 dependency failed.".into(),
    };

    assert_eq!(error.code(), Some(Code::DiskFull));
    let Fault::Failed(diagnostic) = error.fault() else {
        panic!("a failed command is a failure");
    };
    assert_eq!(diagnostic.code(), Code::DiskFull);
    assert_eq!(
        diagnostic.message,
        "the disk that holds the Nix store is full"
    );
    let Some(Detail::Command(command)) = &diagnostic.causes[0].detail else {
        panic!("the command is the cause");
    };
    assert!(command.output_tail.contains("-home-manager-path.drv'"));
    assert!(!command.output_tail.contains("1 dependency failed"));
}

#[test]
fn a_download_nix_gave_up_on_is_a_network_failure_that_names_the_url() {
    let url =
        "https://github.com/NixOS/nixpkgs/archive/efe6f071ede9d21c37462d2d6682d5e670099684.tar.gz";
    let error = Error::Command {
        command: "/nix/var/nix/profiles/default/bin/nix flake prefetch".into(),
        detail: format!(
            "error:\n       … while fetching the input 'github:NixOS/nixpkgs/efe6f071ede9d21c37462d2d6682d5e670099684'\n\n       error: Failed to open archive (Source threw exception: error: unable to download '{url}': Could not connect to server (7))"
        ),
    };

    assert_eq!(error.code(), Some(Code::Network));
    let Fault::Failed(diagnostic) = error.fault() else {
        panic!("a failed command is a failure");
    };
    assert_eq!(diagnostic.code(), Code::Network);
    assert_eq!(diagnostic.message, format!("couldn't download {url}"));
    assert_eq!(
        diagnostic.detail,
        Some(Detail::Network(NetworkDetail { url: url.into() }))
    );
    let Some(Detail::Command(command)) = &diagnostic.causes[0].detail else {
        panic!("the command is the cause");
    };
    assert!(command.output_tail.contains("Could not connect to server"));
}

#[test]
fn a_command_that_is_not_nix_keeps_its_whole_tail() {
    let error = Error::Command {
        command: "git commit".into(),
        detail: "fatal: not a git repository".into(),
    };

    assert_eq!(error.code(), Some(Code::CommandFailed));
    let Fault::Failed(diagnostic) = error.fault() else {
        panic!("a failed command is a failure");
    };
    assert!(matches!(
        &diagnostic.detail,
        Some(Detail::Command(command)) if command.output_tail == "fatal: not a git repository"
    ));
}

#[test]
fn an_effect_error_is_diagnosed_as_the_failure_it_carries() {
    for failure in every_failure() {
        let error = Error::Effect(Box::new(failure.clone()));
        assert_eq!(error.fault(), failure.fault(), "{failure:?}");
        if failure != Failure::Cancelled {
            assert_eq!(
                error.to_string(),
                diagnostic(&failure).message,
                "{failure:?}"
            );
        }
    }
}
