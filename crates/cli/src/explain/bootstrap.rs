//! What `mix bootstrap` says when it cannot finish.

use mix_app::bootstrap::{Error, Host};

use super::{Diagnostic, core_error};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix bootstrap";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(error) => describe(error, COMMAND),
        None => Diagnostic::new(error.to_string()),
    }
}

/// The words for a bootstrap failure, whichever command hit it.
///
/// `mix install` activates a profile through the same machinery, so it reads most of these too;
/// what changes is the command the reader is told to run again.
pub(crate) fn describe(error: &Error, command: &str) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command),

        Error::Activation(e) => super::activation::describe(e, command, None),

        Error::Network(_) => Diagnostic::hinting(
            "could not fetch the pinned nix archive",
            "Check your network connection and proxy settings, or point `--mirror` at a \
             reachable URL",
        ),

        Error::Integrity { artifact, detail } => Diagnostic::hinting(
            format!("{artifact} is not what it was pinned to be: {detail}"),
            "The download was corrupted or the mirror is serving something else; retry, and \
             use the default mirror to rule it out",
        ),

        Error::UnsupportedTarget(target) => Diagnostic::hinting(
            format!("no pinned nix build exists for {target}"),
            "`mix` bootstraps x86_64 and aarch64 Linux",
        ),

        Error::Target(e) => super::target::describe(e, command),

        Error::Decompression(detail) => Diagnostic::hinting(
            format!("the nix archive could not be decompressed: {detail}"),
            format!("The download was truncated or corrupted; run `{command}` again"),
        ),

        Error::MalformedArchive(detail) => Diagnostic::hinting(
            format!("the nix archive is not laid out as expected: {detail}"),
            "The mirror is serving an archive `mix` does not know how to unpack",
        ),

        Error::NotRoot(what) => Diagnostic::hinting(
            format!("root privileges are required to {what}"),
            format!("Re-run it with sudo:\n\x20 sudo {command}"),
        ),

        Error::UnsupportedHost => Diagnostic::hinting(
            "this system already manages its own environment natively",
            "`mix` is designed for standard Linux distributions and is not needed on NixOS",
        ),

        Error::UnsupportedKernel => Diagnostic::hinting(
            "WSL1 does not provide the real Linux kernel that sandboxed builds need",
            "To upgrade this distro to WSL2, run from Windows PowerShell:\n\
             \x20 wsl --set-version <distro> 2",
        ),

        Error::SystemdNotReady { host: Host::Wsl } => Diagnostic::hinting(
            "systemd is not active, and `mix` needs it to run the nix daemon",
            "On WSL2 systemd is off by default: add `[boot]` with `systemd=true` to \
             `/etc/wsl.conf`, run `wsl.exe --shutdown` from Windows, then reopen the distro",
        ),

        Error::SystemdNotReady { host: Host::Native } => Diagnostic::hinting(
            "systemd is not active, and `mix` needs it to run the nix daemon",
            format!(
                "`/run/systemd/system` is missing or PID 1 is not systemd; check that systemd \
                 is installed and set as your init system, then run `{command}` again"
            ),
        ),

        Error::AlreadyManaged => Diagnostic::hinting(
            "an existing, unmanaged nix installation was detected on this system",
            "`mix` requires a dedicated environment to manage its own reproducible runtime; \
             uninstall the existing installation or remove `/nix`, then retry:\n\
             \x20 sudo rm -rf /nix",
        ),

        Error::CrossDeviceStore { path } => Diagnostic::hinting(
            format!(
                "{} cannot be moved into `/nix/store`: the two are on different filesystems",
                path.display()
            ),
            "`mix` stages packages under /nix and renames them into /nix/store, which requires \
             one filesystem; remove any separate mount at /nix/store (e.g. a custom fstab \
             entry) and retry",
        ),

        // The rollback is context the failure was wrapped in: the cause still decides what the
        // reader is told, and the cleanup is what they are warned about instead of the usual way
        // out.
        Error::Rollback { cause, summary } => Diagnostic::hinting(
            describe(cause, command).summary,
            format!("{summary}\nThe system may need manual cleanup"),
        ),

        Error::Interrupted => {
            Diagnostic::new("interrupted; every partially applied change was rolled back")
        }
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

        assert!(message.contains("root privileges are required to bootstrap"));
        assert!(message.contains("sudo mix bootstrap"));
    }

    #[test]
    fn a_network_failure_points_at_the_mirror() {
        let message = message(Error::Network("connection reset".into()));

        assert!(message.contains("--mirror"));
    }

    #[test]
    fn systemd_on_wsl_is_turned_on_differently_than_on_a_distro() {
        assert!(message(Error::SystemdNotReady { host: Host::Wsl }).contains("/etc/wsl.conf"));
        assert!(message(Error::SystemdNotReady { host: Host::Native }).contains("init system"));
    }

    #[test]
    fn a_cross_device_store_explains_the_move_it_could_not_make() {
        let message = message(Error::CrossDeviceStore {
            path: "/nix/store/pkg-a".into(),
        });

        assert!(message.contains("/nix/store/pkg-a"));
        assert!(message.contains("fstab"));
    }

    /// A rollback says what the failure was, and warns about the mess instead of the way out.
    #[test]
    fn a_failed_rollback_keeps_the_cause_and_warns_about_the_cleanup() {
        let message = message(Error::Rollback {
            cause: Box::new(Error::UnsupportedHost),
            summary: "1 rollback step(s) failed: nixbld group: exit 1".to_string(),
        });

        assert!(message.starts_with("this system already manages its own environment"));
        assert!(message.contains("nixbld group: exit 1"));
        assert!(message.contains("manual cleanup"));
    }
}
