use std::fmt::Display;

use mix_events::Fault;
use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{Code, Diagnostic as Wire, Host, Unfixable as WireUnfixable};
use mix_shell::target::Unfixable;

use super::{Diagnostic, bug, failed};

pub(crate) struct Context<'a> {
    pub(crate) command: &'a str,
    pub(crate) action: &'a dyn Display,
}

pub(crate) const DAMAGED: &str = "the downloaded setup files are damaged";

pub(crate) fn render(fault: &Fault, context: &Context<'_>) -> Diagnostic {
    match fault {
        Fault::Cancelled {
            rolled_back: true, ..
        } => Diagnostic::new("stopped; everything it had changed was undone"),
        Fault::Cancelled { .. } => Diagnostic::new("interrupted before it could finish"),
        Fault::Failed(diagnostic) => failure(diagnostic, context),
    }
}

pub(crate) fn rpc_fault(error: &mix_rpc::Error) -> Fault {
    use mix_rpc::Error;

    let code = match error {
        Error::Spawn(_) | Error::Launch(_) | Error::Connect(_) | Error::Refused(_) => {
            Code::PrivilegesUnavailable
        }
        Error::Ended => Code::WorkerEnded,
        Error::Malformed(_) | Error::NotAConnection(_) => Code::Internal,
    };
    mix_core::diagnose::failed(code, error.to_string(), None)
}

pub(crate) fn unfixable_of(reason: i32) -> Option<Unfixable> {
    match WireUnfixable::try_from(reason).ok()? {
        WireUnfixable::NotADirectory => Some(Unfixable::NotADirectory),
        WireUnfixable::MissingUser => Some(Unfixable::MissingUser),
        WireUnfixable::MissingRuntime => Some(Unfixable::MissingRuntime),
        WireUnfixable::Unspecified => None,
    }
}

pub(crate) fn unfixable(reason: Unfixable) -> &'static str {
    match reason {
        Unfixable::NotADirectory => "Remove it, then run `mix repair` again",
        Unfixable::MissingUser => "Recreate the user, or ignore this if it was removed on purpose",
        Unfixable::MissingRuntime => "Run `mix bootstrap` to reinstall it",
    }
}

fn packages(diagnostic: &Wire) -> &[String] {
    match &diagnostic.detail {
        Some(Detail::Packages(detail)) => &detail.packages,
        _ => &[],
    }
}

fn bad_name(name: &str) -> Diagnostic {
    Diagnostic::hinting(
        format!("\"{name}\" isn't a valid package name"),
        "Package names look like `ripgrep` or `python3`",
    )
}

fn protected(packages: &[String]) -> Diagnostic {
    let hint = if packages.len() == 1 {
        "`mix` needs it to work"
    } else {
        "`mix` needs them to work"
    };
    Diagnostic::hinting(
        format!("`{}` can't be removed", packages.join("`, `")),
        hint,
    )
}

fn failure(diagnostic: &Wire, context: &Context<'_>) -> Diagnostic {
    match plain(diagnostic.code(), context) {
        Some(words) => words,
        None => detailed(diagnostic, context),
    }
}

pub(crate) fn render_error<E: mix_events::Diagnose + ?Sized>(
    error: &E,
    context: &Context<'_>,
) -> Diagnostic {
    match error.code().and_then(|code| plain(code, context)) {
        Some(words) => words,
        None => render(&error.fault(), context),
    }
}

fn plain(code: Code, context: &Context<'_>) -> Option<Diagnostic> {
    let command = context.command;
    match code {
        Code::Locked => Some(Diagnostic::hinting_parts(
            "another `mix` command is already running",
            &["Wait for it to finish, then run `", command, "` again"],
        )),
        Code::LockMissing => Some(Diagnostic::hinting(
            "`mix` isn't set up yet",
            "Run `mix bootstrap` first",
        )),
        Code::Io | Code::CommandFailed | Code::SpawnFailed => Some(failed(context.action)),
        Code::Internal | Code::Unspecified => Some(bug()),
        Code::Network => Some(Diagnostic::hinting(
            "couldn't download required setup files",
            format!("Check your internet connection, then run `{command}` again"),
        )),
        Code::Integrity | Code::Decompression => Some(Diagnostic::hinting(
            DAMAGED,
            format!("Run `{command}` again to download them again"),
        )),
        Code::MalformedArchive => Some(Diagnostic::hinting(
            "the downloaded setup files aren't in the expected format",
            "If you use `--mirror`, check that it serves the right files",
        )),
        Code::NotRoot => Some(Diagnostic::hinting(
            "setting up `mix` needs administrator rights",
            format!("Run it again with sudo:\n\x20 sudo {command}"),
        )),
        Code::UnsupportedHost => Some(Diagnostic::hinting(
            "this system is NixOS, which already does what `mix` does",
            "You don't need `mix` here",
        )),
        Code::UnsupportedKernel => Some(Diagnostic::hinting(
            "`mix` needs WSL 2, and this is WSL 1",
            "Upgrade it from Windows PowerShell:\n\x20 wsl --set-version <distro> 2",
        )),
        Code::SystemdUnreachable => Some(Diagnostic::hinting(
            "`mix` couldn't reach systemd",
            format!(
                "Check that the system bus is running with `systemctl status dbus`, then run \
                 `{command}` again"
            ),
        )),
        Code::AlreadyManaged => Some(Diagnostic::hinting(
            "Nix is already installed on this system, and `mix` needs to set up its own",
            format!(
                "Uninstall it first, then run `{command}` again. Uninstalling removes everything \
                 you installed with it"
            ),
        )),
        Code::CrossDeviceStore => Some(Diagnostic::hinting(
            "`/nix/store` is on a different disk than `/nix`, and `mix` needs them on the same one",
            format!("Remove the separate mount for `/nix/store`, then run `{command}` again"),
        )),
        Code::NewerState => Some(Diagnostic::hinting(
            "this version of `mix` is older than the one that set up your packages",
            "Update `mix` using your original install method, or visit \
             https://github.com/recregt/mix",
        )),
        Code::RootNotAllowed => Some(Diagnostic::hinting(
            format!("`{command}` can't be run as root"),
            "Run it again without sudo",
        )),
        Code::NotBootstrapped => Some(Diagnostic::hinting(
            "`mix` isn't set up for you yet",
            "Run `mix bootstrap` first",
        )),
        Code::PrivilegesUnavailable => Some(Diagnostic::hinting(
            format!("couldn't get administrator rights to {}", context.action),
            "Make sure your account can use sudo, then try again",
        )),
        Code::WorkerEnded => Some(Diagnostic::hinting(
            format!("stopped before it could {}", context.action),
            "Run the same command again to finish; it picks up where it stopped",
        )),
        Code::PermissionDenied
        | Code::Conflict
        | Code::InvalidMirror
        | Code::UnsupportedTarget
        | Code::Unrepairable
        | Code::SystemdNotReady
        | Code::UnitFailed
        | Code::RollbackIncomplete
        | Code::InvalidPackage
        | Code::InvalidState
        | Code::ProtectedPackage => None,
    }
}

fn detailed(diagnostic: &Wire, context: &Context<'_>) -> Diagnostic {
    let command = context.command;
    let detail = diagnostic.detail.as_ref();
    match diagnostic.code() {
        Code::PermissionDenied => match detail {
            Some(Detail::Io(io)) => Diagnostic::hinting_parts(
                &format!("no permission to use {}", io.path),
                &["Check who owns it, then run `", command, "` again"],
            ),
            _ => failed(context.action),
        },
        Code::Conflict => match detail {
            Some(Detail::Conflict(conflict)) => Diagnostic::hinting(
                format!(
                    "{} changed while mix was working, so mix left it alone",
                    conflict.subject
                ),
                format!("Run `{command}` again; it starts from what is there now"),
            ),
            _ => failed(context.action),
        },
        Code::InvalidMirror => Diagnostic::hinting(
            format!("the mirror settings aren't valid: {}", diagnostic.message),
            "Pass `--mirror` as an http or https URL, and `--mirror-key` as a single <name>:<key> entry",
        ),
        Code::UnsupportedTarget => {
            let target = match detail {
                Some(Detail::Target(target)) if target.os.is_empty() => target.arch.clone(),
                Some(Detail::Target(target)) => format!("{}-{}", target.arch, target.os),
                _ => String::new(),
            };
            Diagnostic::hinting(
                format!("`mix` doesn't support this system ({target}) yet"),
                "It runs on 64-bit Intel, AMD and ARM Linux",
            )
        }
        Code::Unrepairable => match detail {
            Some(Detail::Unrepairable(detail)) => match unfixable_of(detail.reason) {
                Some(reason) => {
                    Diagnostic::hinting(format!("{}: {reason}", detail.artifact), unfixable(reason))
                }
                None => bug(),
            },
            _ => bug(),
        },
        Code::SystemdNotReady => match detail {
            Some(Detail::Host(host)) if host.host() == Host::Wsl => Diagnostic::hinting(
                "`mix` needs systemd, and it isn't running",
                "Turn it on: add `[boot]` with `systemd=true` to `/etc/wsl.conf`, run \
                 `wsl.exe --shutdown` from Windows, then reopen the distro",
            ),
            _ => Diagnostic::hinting(
                "`mix` needs systemd, and it isn't running",
                format!("Make sure systemd is your init system, then run `{command}` again"),
            ),
        },
        Code::UnitFailed => match detail {
            Some(Detail::Unit(unit)) => Diagnostic::hinting(
                format!("systemd couldn't {} `{}`", unit.operation, unit.unit),
                match &unit.invocation {
                    Some(id) => format!(
                        "See why with:\n\x20 journalctl _SYSTEMD_INVOCATION_ID={id}\nthen run \
                         `{command}` again"
                    ),
                    None => format!(
                        "See why with:\n\x20 journalctl -u {}\nthen run `{command}` again",
                        unit.unit
                    ),
                },
            ),
            _ => failed(context.action),
        },
        Code::RollbackIncomplete => Diagnostic::hinting(
            match diagnostic.causes.first() {
                Some(cause) => failure(cause, context).summary(),
                None => "interrupted before it could finish".into(),
            },
            "Some changes couldn't be undone. Run `mix doctor` to see what's left",
        ),
        Code::InvalidPackage => match packages(diagnostic) {
            [name, ..] => bad_name(name),
            [] => bug(),
        },
        Code::InvalidState => match packages(diagnostic) {
            [name, ..] => bad_name(name),
            [] => bug(),
        },
        Code::ProtectedPackage => protected(packages(diagnostic)),
        code => plain(code, context).unwrap_or_else(bug),
    }
}
