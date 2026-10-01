//! What `mix bootstrap` says when it cannot finish.

use mix_shell::ops::bootstrap::Error;

use super::{Context, Diagnostic, failed};

/// How the command is spelled when the reader is told to run it again.
pub(crate) const COMMAND: &str = "mix bootstrap";

pub(crate) const ACTION: &str = "finish setting up `mix`";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    if let Some(error) = error.downcast_ref::<mix_rpc::Error>() {
        return super::privileged(error, &ACTION);
    }
    if let Some(failed) = error.downcast_ref::<crate::remote::client::Failed>() {
        return super::outcome(Some(&failed.request), &failed.fault);
    }
    match error.downcast_ref::<Error>() {
        Some(error) => describe(error, COMMAND),
        None => failed(&ACTION),
    }
}

pub(crate) fn describe(error: &Error, command: &str) -> Diagnostic {
    super::render::render_error(
        error,
        &Context {
            command,
            action: &ACTION,
        },
    )
}

#[cfg(test)]
mod tests {
    use mix_shell::ops::bootstrap::Host;

    use super::super::render::damaged;
    use super::*;

    fn message(error: Error) -> String {
        describe(&error, COMMAND).message()
    }

    #[test]
    fn a_missing_privilege_names_the_command_to_re_run() {
        let message = message(Error::NotRoot("bootstrap the managed environment"));

        assert!(message.starts_with("setting up `mix` needs administrator rights"));
        assert!(message.contains("sudo mix bootstrap"));
    }

    #[test]
    fn a_network_failure_points_at_the_connection() {
        let message = message(Error::Network("connection reset".into()));

        assert!(message.contains("couldn't download required setup files"));
        assert!(message.contains("internet connection"));
        assert!(!message.contains("connection reset"));
    }

    #[test]
    fn a_damaged_download_reads_the_same_however_it_was_caught() {
        let integrity = message(Error::Integrity {
            artifact: "nix archive".into(),
            detail: "sha256 mismatch".into(),
        });
        let decompression = message(Error::Decompression("unexpected end".into()));

        assert_eq!(integrity, decompression);
        assert!(integrity.starts_with(damaged().as_str()));
    }

    #[test]
    fn systemd_on_wsl_is_turned_on_differently_than_on_a_distro() {
        assert!(message(Error::SystemdNotReady { host: Host::Wsl }).contains("/etc/wsl.conf"));
        assert!(message(Error::SystemdNotReady { host: Host::Native }).contains("init system"));
    }

    #[test]
    fn a_unit_failure_names_the_operation_and_the_run_to_read() {
        let unit = |invocation: Option<&str>| {
            message(Error::Unit {
                operation: "start".into(),
                unit: "nix-daemon.socket".into(),
                detail: "job failed".into(),
                invocation: invocation.map(str::to_string),
            })
        };

        assert!(unit(None).starts_with("systemd couldn't start `nix-daemon.socket`"));
        assert!(unit(None).contains("journalctl -u nix-daemon.socket"));
        assert!(unit(Some("ab12")).contains("journalctl _SYSTEMD_INVOCATION_ID=ab12"));
    }

    #[test]
    fn an_unreachable_systemd_is_not_mistaken_for_a_missing_one() {
        let message = message(Error::SystemdUnreachable);

        assert!(message.contains("systemctl status dbus"));
        assert!(!message.contains("init system"));
    }

    #[test]
    fn an_existing_nix_is_never_met_with_a_command_that_deletes_it() {
        let message = message(Error::AlreadyManaged);

        assert!(!message.contains("rm -rf"));
        assert!(message.contains("removes everything you installed with it"));
    }

    #[test]
    fn a_cross_device_store_names_the_mount_to_remove() {
        let message = message(Error::CrossDeviceStore {
            path: "/nix/store/pkg-a".into(),
        });

        assert!(message.contains("different disk"));
        assert!(message.contains("separate mount for `/nix/store`"));
    }

    #[test]
    fn a_failed_rollback_keeps_the_cause_and_says_where_to_look() {
        let message = message(Error::Rollback {
            cause: Box::new(Error::UnsupportedHost),
            summary: "1 rollback step(s) failed: nixbld group: exit 1".to_string(),
        });

        assert!(message.starts_with("this system is NixOS"));
        assert!(message.contains("mix doctor"));
        assert!(!message.contains("nixbld group"));
    }

    #[test]
    fn nothing_mix_does_internally_reaches_the_reader() {
        let errors = [
            Error::Network("x".into()),
            Error::Decompression("x".into()),
            Error::MalformedArchive("x".into()),
            Error::UnsupportedTarget("armv7l-linux".into()),
            Error::NotRoot("x"),
        ];
        for error in errors {
            let message = message(error);
            for word in ["pinned", "archive", "daemon", "nix store", "derivation"] {
                assert!(!message.contains(word), "{word:?} leaked into {message:?}");
            }
        }
    }
}
