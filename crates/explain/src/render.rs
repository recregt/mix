use std::fmt::Display;

use mix_core::health::Unfixable;
use mix_events::Fault;
use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{Code, Diagnostic as Wire, Host, Unfixable as WireUnfixable};
use mix_ui::{Help, help, help_around, note, phrase, phrase_parts};

use super::{Diagnostic, bug, failed};

pub(crate) struct Context<'a> {
    pub(crate) command: &'a str,
    pub(crate) action: &'a dyn Display,
}

pub(crate) fn render(fault: &Fault, context: &Context<'_>) -> Diagnostic {
    match fault {
        Fault::Cancelled {
            rolled_back: true, ..
        } => Diagnostic::new(phrase!(
            "cancelled, and everything it had changed was put back"
        )),
        Fault::Cancelled { .. } => Diagnostic::new(phrase!("cancelled before it could finish")),
        Fault::Failed(diagnostic) => failure(diagnostic, context),
    }
}

pub fn warning(diagnostic: &Wire) -> Diagnostic {
    let context = Context {
        command: "mix",
        action: &"",
    };
    match Code::try_from(diagnostic.code) {
        Ok(code @ (Code::JournalUnwritable | Code::CleanupIncomplete | Code::GitRecordFailed)) => {
            plain(code, &context).unwrap_or_else(bug)
        }
        _ => bug(),
    }
}

pub(crate) fn unfixable_of(reason: i32) -> Option<Unfixable> {
    match WireUnfixable::try_from(reason).ok()? {
        WireUnfixable::NotADirectory => Some(Unfixable::NotADirectory),
        WireUnfixable::MissingUser => Some(Unfixable::MissingUser),
        WireUnfixable::MissingRuntime => Some(Unfixable::MissingRuntime),
        WireUnfixable::Unspecified => None,
    }
}

pub(crate) fn unfixable(reason: Unfixable) -> Help {
    match reason {
        Unfixable::NotADirectory => help!("remove it, then run `mix repair` again"),
        Unfixable::MissingUser => {
            help!("recreate the user, or ignore this if it was removed on purpose")
        }
        Unfixable::MissingRuntime => help!("run `mix bootstrap` to reinstall it"),
    }
}

fn unfixable_reason(reason: Unfixable) -> &'static str {
    match reason {
        Unfixable::NotADirectory => "exists but is not a directory",
        Unfixable::MissingUser => "the user no longer exists",
        Unfixable::MissingRuntime => "missing, and `mix repair` can't restore it",
    }
}

pub(crate) fn unrepairable(artifact: &str, reason: Unfixable) -> Diagnostic {
    Diagnostic::new(phrase_parts![
        "",
        artifact,
        ": ",
        unfixable_reason(reason),
        ""
    ])
    .help(unfixable(reason))
}

fn packages(diagnostic: &Wire) -> &[String] {
    match &diagnostic.detail {
        Some(Detail::Packages(detail)) => &detail.packages,
        _ => &[],
    }
}

fn bad_name(name: &str) -> Diagnostic {
    Diagnostic::new(phrase!("\"{name}\" isn't a valid package name"))
        .note(note!("package names look like `ripgrep` or `python3`"))
}

fn protected(packages: &[String]) -> Diagnostic {
    let note = if packages.len() == 1 {
        note!("`mix` needs it to work")
    } else {
        note!("`mix` needs them to work")
    };
    Diagnostic::new(phrase!("`{}` can't be removed", packages.join("`, `"))).note(note)
}

fn failure(diagnostic: &Wire, context: &Context<'_>) -> Diagnostic {
    match plain(diagnostic.code(), context) {
        Some(words) => words,
        None => detailed(diagnostic, context),
    }
}

pub(crate) fn damaged() -> mix_ui::Phrase {
    phrase!("the downloaded setup files are damaged")
}

fn plain(code: Code, context: &Context<'_>) -> Option<Diagnostic> {
    let command = context.command;
    let words = match code {
        Code::LockMissing => Diagnostic::new(phrase!("`mix` isn't set up yet"))
            .help(help!("run `mix bootstrap` first")),
        Code::Io | Code::CommandFailed | Code::SpawnFailed => failed(context.action),
        Code::Internal | Code::Unspecified => bug(),
        Code::JournalUnwritable => Diagnostic::new(phrase!("`mix` couldn't record its progress"))
            .note(note!(
                "if this command is interrupted, `mix repair` may not be able to finish it"
            )),
        Code::CleanupIncomplete => Diagnostic::new(phrase!(
            "`mix` couldn't clean up after an unfinished step"
        ))
        .help(help!(
            "run `mix doctor` to see what is left, and `mix repair` to put back what it can"
        )),
        Code::GitRecordFailed => {
            Diagnostic::new(phrase!("the change was made but not recorded in git"))
                .help(help!("run `mix repair` to record it"))
        }
        Code::Network => Diagnostic::hinting(
            phrase!("couldn't download required setup files"),
            help_around!("check your internet connection, then run `", "` again"),
            command,
        ),
        Code::Integrity | Code::Decompression => Diagnostic::hinting(
            damaged(),
            help_around!("run `", "` again to download them again"),
            command,
        ),
        Code::MalformedArchive => Diagnostic::new(phrase!(
            "the downloaded setup files aren't in the expected format"
        ))
        .help(help!(
            "check that the mirror given with `--mirror` serves the right files"
        )),
        Code::NotRoot => Diagnostic::hinting(
            phrase!("setting up `mix` needs administrator rights"),
            help_around!("run it again with sudo:\n\x20 sudo ", ""),
            command,
        ),
        Code::UnsupportedHost => Diagnostic::new(phrase!(
            "this system is NixOS, which already does what `mix` does"
        ))
        .note(note!("`mix` has nothing to do here")),
        Code::UnsupportedKernel => Diagnostic::new(phrase!("`mix` needs WSL 2, and this is WSL 1"))
            .help(help!(
                "upgrade it from Windows PowerShell:\n\x20 wsl --set-version <distro> 2"
            )),
        Code::SystemdUnreachable => Diagnostic::hinting(
            phrase!("`mix` couldn't reach systemd"),
            help_around!(
                "check that the system bus is running with `systemctl status dbus`, then run \
                 `",
                "` again"
            ),
            command,
        ),
        Code::AlreadyManaged => Diagnostic::new(phrase!(
            "this system already has Nix, set up by something other than `mix`"
        ))
        .note(note!(
            "`mix` needs to set up its own, and uninstalling the existing one removes everything \
             you installed with it"
        ))
        .help_around(help_around!("uninstall it, then run `", "` again"), command),
        Code::CrossDeviceStore => Diagnostic::hinting(
            phrase!(
                "`/nix/store` is on a different disk than `/nix`, and `mix` needs them on the same one"
            ),
            help_around!(
                "remove the separate mount for `/nix/store`, then run `",
                "` again"
            ),
            command,
        ),
        Code::NewerState => Diagnostic::new(phrase!(
            "this version of `mix` is older than the one that set up your packages"
        ))
        .help(help!(
            "update `mix` using your original install method, or visit \
             https://github.com/recregt/mix"
        )),
        Code::RootNotAllowed => Diagnostic::new(phrase!("`{command}` can't be run as root"))
            .help(help!("run it again without sudo")),
        Code::NotBootstrapped => Diagnostic::new(phrase!("`mix` isn't set up for you yet"))
            .help(help!("run `mix bootstrap` first")),
        Code::PrivilegesUnavailable => Diagnostic::new(phrase!(
            "couldn't get administrator rights to {}",
            context.action
        ))
        .help(help!("make sure your account can use sudo, then try again")),
        Code::WorkerEnded => Diagnostic::new(phrase!("stopped before it could {}", context.action))
            .help(help!(
                "run the same command again to pick up where it stopped"
            )),
        Code::VersionMismatch => Diagnostic::new(phrase!(
            "the `mix` program was replaced while this command was starting"
        ))
        .note(note!("nothing was changed"))
        .help(help!("run the same command again")),
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
        | Code::ProtectedPackage
        | Code::UnknownPackage
        | Code::BuildFailed => return None,
    };
    Some(words)
}

fn detailed(diagnostic: &Wire, context: &Context<'_>) -> Diagnostic {
    let command = context.command;
    let detail = diagnostic.detail.as_ref();
    match diagnostic.code() {
        Code::PermissionDenied => match detail {
            Some(Detail::Io(io)) => Diagnostic::hinting(
                phrase!("no permission to use {}", io.path),
                help_around!("check who owns it, then run `", "` again"),
                command,
            ),
            _ => failed(context.action),
        },
        Code::Conflict => match detail {
            Some(Detail::Conflict(conflict)) => Diagnostic::hinting(
                phrase!(
                    "{} changed while mix was working, so mix left it alone",
                    conflict.subject
                ),
                help_around!("run `", "` again to start from what is there now"),
                command,
            ),
            _ => failed(context.action),
        },
        Code::InvalidMirror => Diagnostic::new(phrase!(
            "the mirror settings aren't valid: {}",
            diagnostic.message
        ))
        .help(help!(
            "pass `--mirror` as an http or https URL, and `--mirror-key` as a single \
             <name>:<key> entry"
        )),
        Code::UnsupportedTarget => {
            let target = match detail {
                Some(Detail::Target(target)) if target.os.is_empty() => target.arch.clone(),
                Some(Detail::Target(target)) => format!("{}-{}", target.arch, target.os),
                _ => String::new(),
            };
            Diagnostic::new(phrase!("`mix` doesn't support this system ({target}) yet"))
                .note(note!("`mix` runs on 64-bit Intel, AMD and ARM Linux"))
        }
        Code::Unrepairable => match detail {
            Some(Detail::Unrepairable(detail)) => match unfixable_of(detail.reason) {
                Some(reason) => unrepairable(&detail.artifact, reason),
                None => bug(),
            },
            _ => bug(),
        },
        Code::SystemdNotReady => {
            let summary = phrase!("`mix` needs systemd, and it isn't running");
            match detail {
                Some(Detail::Host(host)) if host.host() == Host::Wsl => Diagnostic::new(summary)
                    .help(help!(
                        "turn it on: add `[boot]` with `systemd=true` to `/etc/wsl.conf`, run \
                         `wsl.exe --shutdown` from Windows, then reopen the distro"
                    )),
                _ => Diagnostic::new(summary).help_around(
                    help_around!(
                        "make sure systemd is your init system, then run `",
                        "` again"
                    ),
                    command,
                ),
            }
        }
        Code::UnitFailed => match detail {
            Some(Detail::Unit(unit)) => {
                let summary = phrase!("systemd couldn't {} `{}`", unit.operation, unit.unit);
                let help = match &unit.invocation {
                    Some(id) => help!(
                        "see why with:\n\x20 journalctl _SYSTEMD_INVOCATION_ID={id}\nthen run \
                         `{command}` again"
                    ),
                    None => help!(
                        "see why with:\n\x20 journalctl -u {}\nthen run `{command}` again",
                        unit.unit
                    ),
                };
                Diagnostic::new(summary).help(help)
            }
            _ => failed(context.action),
        },
        Code::RollbackIncomplete => Diagnostic::new(match diagnostic.causes.first() {
            Some(cause) => failure(cause, context).summary(),
            None => phrase!("couldn't undo every change"),
        })
        .help(help!(
            "run `mix doctor` to see the changes that couldn't be undone"
        )),
        Code::InvalidPackage => match packages(diagnostic) {
            [name, ..] => bad_name(name),
            [] => bug(),
        },
        Code::UnknownPackage => match packages(diagnostic) {
            [name, ..] => Diagnostic::hinting(
                phrase!("`{name}` isn't a package `mix` can find"),
                help_around!("check the name, then run `", "` again"),
                command,
            ),
            [] => bug(),
        },
        Code::BuildFailed => match packages(diagnostic) {
            [package, ..] => Diagnostic::new(phrase!("couldn't build {package}"))
                .help(help!("run it again with `-vv` to see the whole build")),
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
