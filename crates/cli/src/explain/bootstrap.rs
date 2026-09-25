//! What `mix bootstrap` says when it cannot finish.

use mix_app::bootstrap::{Error, Host};

use super::{Diagnostic, core_error, failed};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix bootstrap";

const ACTION: &str = "finish setting up `mix`";

const DAMAGED: &str = "the downloaded setup files are damaged";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(error) => describe(error, COMMAND),
        None => failed(ACTION),
    }
}

pub(crate) fn describe(error: &Error, command: &str) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command, ACTION),

        Error::Activation(e) => super::activation::describe(e, command, ACTION, None),

        Error::Network(_) => Diagnostic::hinting(
            "couldn't download required setup files",
            format!("Check your internet connection, then run `{command}` again"),
        ),

        Error::Integrity { .. } | Error::Decompression(_) => Diagnostic::hinting(
            DAMAGED,
            format!("Run `{command}` again to download them again"),
        ),

        Error::MalformedArchive(_) => Diagnostic::hinting(
            "the downloaded setup files aren't in the expected format",
            "If you use `--mirror`, check that it serves the right files",
        ),

        Error::UnsupportedTarget(target) => Diagnostic::hinting(
            format!("`mix` doesn't support this system ({target}) yet"),
            "It runs on 64-bit Intel, AMD and ARM Linux",
        ),

        Error::Target(e) => super::target::describe(e, command, ACTION),

        Error::NotRoot(_) => Diagnostic::hinting(
            "setting up `mix` needs administrator rights",
            format!("Run it again with sudo:\n\x20 sudo {command}"),
        ),

        Error::UnsupportedHost => Diagnostic::hinting(
            "this system is NixOS, which already does what `mix` does",
            "You don't need `mix` here",
        ),

        Error::UnsupportedKernel => Diagnostic::hinting(
            "`mix` needs WSL 2, and this is WSL 1",
            "Upgrade it from Windows PowerShell:\n\x20 wsl --set-version <distro> 2",
        ),

        Error::SystemdNotReady { host: Host::Wsl } => Diagnostic::hinting(
            "`mix` needs systemd, and it isn't running",
            "Turn it on: add `[boot]` with `systemd=true` to `/etc/wsl.conf`, run \
             `wsl.exe --shutdown` from Windows, then reopen the distro",
        ),

        Error::SystemdNotReady { host: Host::Native } => Diagnostic::hinting(
            "`mix` needs systemd, and it isn't running",
            format!("Make sure systemd is your init system, then run `{command}` again"),
        ),

        Error::AlreadyManaged => Diagnostic::hinting(
            "Nix is already installed on this system, and `mix` needs to set up its own",
            format!(
                "Uninstall it first, then run `{command}` again. Uninstalling removes everything \
                 you installed with it"
            ),
        ),

        Error::CrossDeviceStore { .. } => Diagnostic::hinting(
            "`/nix/store` is on a different disk than `/nix`, and `mix` needs them on the same one",
            format!("Remove the separate mount for `/nix/store`, then run `{command}` again"),
        ),

        Error::Rollback { cause, .. } => Diagnostic::hinting(
            describe(cause, command).summary,
            "Some changes couldn't be undone. Run `mix doctor` to see what's left",
        ),

        Error::Interrupted => Diagnostic::new("stopped; everything it had changed was undone"),
    }
}

#[cfg(test)]
mod tests {
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
        assert!(integrity.starts_with(DAMAGED));
    }

    #[test]
    fn systemd_on_wsl_is_turned_on_differently_than_on_a_distro() {
        assert!(message(Error::SystemdNotReady { host: Host::Wsl }).contains("/etc/wsl.conf"));
        assert!(message(Error::SystemdNotReady { host: Host::Native }).contains("init system"));
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
