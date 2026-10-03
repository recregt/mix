use mix_events::Diagnose;
use mix_events::v1::{CommandDetail, Diagnostic as Wire, diagnostic::Detail};
use mix_shell::ops::bootstrap::{Error as BootstrapError, Host};
use mix_shell::ops::remove::Error as RemoveError;
use mix_shell::profile::change::Error as ChangeError;
use mix_shell::profile::state::Invalid;
use mix_shell::target::{Error as TargetError, Unfixable};

use super::render::damaged;
use super::*;

fn said(command: &str, action: &str, error: &impl Diagnose) -> String {
    outcome(command, &action, &error.fault()).message()
}

const BOOTSTRAP: (&str, &str) = ("mix bootstrap", "finish setting up `mix`");
const INSTALL: (&str, &str) = ("mix install", "install ripgrep");
const REMOVE: (&str, &str) = ("mix remove", "remove git");
const REPAIR: (&str, &str) = ("mix repair", "finish the repair");
const DOCTOR: (&str, &str) = ("mix doctor", "finish the health check");
const CLEAN: (&str, &str) = ("mix clean", "clean up your profile");

fn says((command, action): (&str, &str), error: &impl Diagnose) -> String {
    said(command, action, error)
}

#[test]
fn a_message_lists_the_summary_then_the_note_then_the_help() {
    let diagnostic = Diagnostic::new(phrase!("it failed"))
        .help(help!("run it again"))
        .note(mix_ui::note!("it was busy"));

    assert_eq!(diagnostic.message(), "it failed\nit was busy\nrun it again");
}

#[test]
fn a_failure_without_a_note_or_help_is_its_summary() {
    assert_eq!(Diagnostic::new(phrase!("it failed")).message(), "it failed");
}

#[test]
fn a_refused_path_says_whose_permission_is_missing() {
    let error = mix_core::Error::Io {
        path: "/nix/store".into(),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };

    let message = says(DOCTOR, &error);

    assert!(message.starts_with("no permission to use /nix/store"));
    assert!(message.contains("check who owns it"));
}

#[test]
fn a_failed_step_says_what_could_not_be_done_and_keeps_the_internals_out() {
    let error = mix_core::Error::Command {
        command: "/nix/var/nix/profiles/default/bin/nix build path:/home/ada".to_string(),
        detail: "error: out of disk space".to_string(),
    };

    assert_eq!(
        says(INSTALL, &error),
        "couldn't install ripgrep\nrun it again with `-v` to see what went wrong"
    );
}

#[test]
fn a_bug_is_called_a_bug_and_says_where_to_report_it() {
    let message = outcome(
        INSTALL.0,
        &INSTALL.1,
        &mix_events::Fault::failed(mix_events::v1::Code::Internal, "oops", None),
    )
    .message();

    assert!(message.contains("report this bug"));
    assert!(message.contains("github.com/recregt/mix/issues"));
    assert!(!message.contains("oops"));
}

#[test]
fn a_missing_lock_sends_the_reader_to_bootstrap() {
    let error = mix_core::Error::LockMissing {
        path: "/var/lib/mix/lock".into(),
    };

    assert_eq!(
        says(INSTALL, &error),
        "`mix` isn't set up yet\nrun `mix bootstrap` first"
    );
}

#[test]
fn evidence_is_the_programs_own_words_and_each_causes_message() {
    let fault = mix_events::Fault::Failed(Wire {
        code: mix_events::v1::Code::BuildFailed as i32,
        message: "the summary is worded elsewhere".into(),
        causes: vec![
            Wire {
                message: "`nix build` failed".into(),
                detail: Some(Detail::Command(CommandDetail {
                    output_tail: "error: Cannot build 'hello'.\n".into(),
                    ..CommandDetail::default()
                })),
                ..Wire::default()
            },
            Wire {
                message: "/var/lib/mix/journal/r1: PermissionDenied".into(),
                ..Wire::default()
            },
        ],
        ..Wire::default()
    });

    assert_eq!(
        evidence(&fault),
        [
            "error: Cannot build 'hello'.",
            "/var/lib/mix/journal/r1: PermissionDenied"
        ]
    );
}

#[test]
fn a_failure_mix_has_no_words_for_keeps_its_producers_words() {
    for code in [
        mix_events::v1::Code::Internal,
        mix_events::v1::Code::Unspecified,
    ] {
        let fault = mix_events::Fault::Failed(Wire {
            code: code as i32,
            message: "state file ended early".into(),
            ..Wire::default()
        });

        assert_eq!(evidence(&fault), ["state file ended early"], "{code:?}");
    }
}

#[test]
fn a_missing_privilege_names_the_command_to_re_run() {
    let message = says(
        BOOTSTRAP,
        &BootstrapError::NotRoot("bootstrap the managed environment"),
    );

    assert!(message.starts_with("setting up `mix` needs administrator rights"));
    assert!(message.contains("sudo mix bootstrap"));
}

#[test]
fn a_network_failure_points_at_the_connection() {
    let message = says(
        BOOTSTRAP,
        &BootstrapError::Network("connection reset".into()),
    );

    assert!(message.contains("couldn't download required setup files"));
    assert!(message.contains("internet connection"));
    assert!(!message.contains("connection reset"));
}

#[test]
fn a_damaged_download_reads_the_same_however_it_was_caught() {
    let integrity = says(
        BOOTSTRAP,
        &BootstrapError::Integrity {
            artifact: "nix archive".into(),
            detail: "sha256 mismatch".into(),
        },
    );
    let decompression = says(
        BOOTSTRAP,
        &BootstrapError::Decompression("unexpected end".into()),
    );

    assert_eq!(integrity, decompression);
    assert!(integrity.starts_with(damaged().as_str()));
}

#[test]
fn systemd_on_wsl_is_turned_on_differently_than_on_a_distro() {
    assert!(
        says(
            BOOTSTRAP,
            &BootstrapError::SystemdNotReady { host: Host::Wsl }
        )
        .contains("/etc/wsl.conf")
    );
    assert!(
        says(
            BOOTSTRAP,
            &BootstrapError::SystemdNotReady { host: Host::Native }
        )
        .contains("init system")
    );
}

#[test]
fn a_unit_failure_names_the_operation_and_the_run_to_read() {
    let unit = |invocation: Option<&str>| {
        says(
            BOOTSTRAP,
            &BootstrapError::Unit {
                operation: "start".into(),
                unit: "nix-daemon.socket".into(),
                detail: "job failed".into(),
                invocation: invocation.map(str::to_string),
            },
        )
    };

    assert!(unit(None).starts_with("systemd couldn't start `nix-daemon.socket`"));
    assert!(unit(None).contains("journalctl -u nix-daemon.socket"));
    assert!(unit(Some("ab12")).contains("journalctl _SYSTEMD_INVOCATION_ID=ab12"));
}

#[test]
fn an_unreachable_systemd_is_not_mistaken_for_a_missing_one() {
    let message = says(BOOTSTRAP, &BootstrapError::SystemdUnreachable);

    assert!(message.contains("systemctl status dbus"));
    assert!(!message.contains("init system"));
}

#[test]
fn an_existing_nix_is_never_met_with_a_command_that_deletes_it() {
    let message = says(BOOTSTRAP, &BootstrapError::AlreadyManaged);

    assert!(!message.contains("rm -rf"));
    assert!(message.contains("removes everything you installed with it"));
}

#[test]
fn a_cross_device_store_names_the_mount_to_remove() {
    let message = says(
        BOOTSTRAP,
        &BootstrapError::CrossDeviceStore {
            path: "/nix/store/pkg-a".into(),
        },
    );

    assert!(message.contains("different disk"));
    assert!(message.contains("separate mount for `/nix/store`"));
}

#[test]
fn nothing_mix_does_internally_reaches_the_reader() {
    let errors = [
        BootstrapError::Network("x".into()),
        BootstrapError::Decompression("x".into()),
        BootstrapError::MalformedArchive("x".into()),
        BootstrapError::UnsupportedTarget("armv7l-linux".into()),
        BootstrapError::NotRoot("x"),
    ];
    for error in errors {
        let message = says(BOOTSTRAP, &error);
        for word in ["pinned", "archive", "daemon", "nix store", "derivation"] {
            assert!(!message.contains(word), "{word:?} leaked into {message:?}");
        }
    }
}

#[test]
fn running_as_root_names_the_command_and_says_how_to_run_it_instead() {
    for (command, action) in [INSTALL, REMOVE, CLEAN] {
        let message = said(command, action, &ChangeError::NotRoot);

        assert!(message.contains(&format!("`{command}` can't be run as root")));
        assert!(message.contains("without sudo"));
    }
}

#[test]
fn an_older_mix_is_told_how_to_update() {
    let message = says(INSTALL, &ChangeError::NewerState(2));

    assert!(message.contains("older than the one that set up your packages"));
    assert!(message.contains("original install method"));
    assert!(message.contains("https://github.com/recregt/mix"));
}

#[test]
fn a_bad_package_name_reads_the_same_whichever_check_caught_it() {
    let from_state = says(
        INSTALL,
        &ChangeError::InvalidState(Invalid::Package("rip grep".to_string())),
    );
    let from_render = says(
        INSTALL,
        &ChangeError::InvalidPackage(
            mix_nixgen::HomeModule::new(
                "mix",
                std::path::Path::new("/home/mix"),
                mix_nixgen::StateVersion::new_static("24.05"),
            )
            .unwrap()
            .packages(["rip grep"])
            .unwrap_err(),
        ),
    );

    assert_eq!(from_state, from_render);
    assert_eq!(
        from_state,
        "\"rip grep\" isn't a valid package name\npackage names look like `ripgrep` or `python3`"
    );
    assert!(!from_state.to_lowercase().contains("nix"));
}

#[test]
fn a_list_mix_built_wrong_is_reported_as_a_bug() {
    assert!(
        says(INSTALL, &ChangeError::InvalidState(Invalid::Missing("git")))
            .contains("report this bug")
    );
}

#[test]
fn a_reset_package_list_is_told_with_the_way_back() {
    let note = reset().message();
    assert!(note.contains("couldn't be recovered"));
    assert!(note.ends_with("reinstall your packages with `mix install`"));
}

#[test]
fn an_unbootstrapped_user_is_sent_to_bootstrap() {
    let message = says(INSTALL, &ChangeError::NotBootstrapped);

    assert!(message.contains("isn't set up for you yet"));
    assert!(message.contains("mix bootstrap"));
}

#[test]
fn a_protected_package_is_named_and_explained() {
    let message = says(REMOVE, &RemoveError::Protected(vec!["git".to_string()]));

    assert!(message.contains("`git` can't be removed"));
    assert!(message.contains("`mix` needs it to work"));
}

#[test]
fn several_protected_packages_are_each_named() {
    let message = says(
        REMOVE,
        &RemoveError::Protected(vec!["git".to_string(), "curl".to_string()]),
    );

    assert!(message.contains("`git`, `curl` can't be removed"));
}

#[test]
fn a_target_repair_will_not_touch_is_explained_in_its_own_words() {
    let message = says(
        REPAIR,
        &TargetError::Unrepairable {
            artifact: "/nix".to_string(),
            reason: Unfixable::NotADirectory,
        },
    );

    assert!(message.contains("/nix: exists but is not a directory"));
    assert!(message.contains("remove it, then run `mix repair` again"));
}

#[test]
fn a_missing_runtime_is_sent_to_bootstrap() {
    let message = says(
        REPAIR,
        &TargetError::Unrepairable {
            artifact: "default profile".to_string(),
            reason: Unfixable::MissingRuntime,
        },
    );

    assert!(message.contains("`mix repair` can't restore it"));
    assert!(message.contains("mix bootstrap"));
}
