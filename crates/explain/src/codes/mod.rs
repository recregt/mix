use mix_events::code::{kebab, name};
use mix_events::v1::Code;

pub fn list_text(codes: impl IntoIterator<Item = Code>) -> String {
    let codes: Vec<(String, &str)> = codes
        .into_iter()
        .map(|code| {
            let description = explanation(code).description;
            (
                kebab(code),
                description.strip_suffix('.').unwrap_or(description),
            )
        })
        .collect();
    let width = codes.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    let mut out = String::from("Failure codes:");
    for (name, description) in codes {
        out.push_str(&format!("\n    {name:<width$} {description}"));
    }
    out.push_str("\n\n");
    out.push_str(mix_ui::sentence!(
        "Run `mix explain <code>` to read about one of them."
    ));
    out
}

#[derive(Debug, Clone, Copy)]
pub struct Explanation {
    pub description: &'static str,
    pub why: &'static str,
    pub fix: &'static [&'static str],
}

macro_rules! explained {
    ($description:literal, $why:literal, [$($fix:literal),* $(,)?]) => {
        Explanation {
            description: mix_ui::sentence!($description),
            why: mix_ui::prose!($why),
            fix: &[$(mix_ui::instruction!($fix)),*],
        }
    };
}

pub fn explanation(code: Code) -> Explanation {
    match code {
        Code::Unspecified => explained!(
            "A failure arrived without a code.",
            "Every failure `mix` reports carries a code, so one without a code is a bug in `mix`.",
            [
                "report it at https://github.com/recregt/mix/issues with the output of the command run with `-v`"
            ]
        ),
        Code::Internal => explained!(
            "Something went wrong inside `mix` itself.",
            "A task panicked, or two parts of `mix` disagreed about the protocol between them. Nothing on your system caused it.",
            [
                "report it at https://github.com/recregt/mix/issues with the output of the command run with `-v`"
            ]
        ),
        Code::Io => explained!(
            "Reading or writing a file failed for a reason other than permissions.",
            "Common causes are a missing directory, a full disk, or a read-only filesystem.",
            ["run the command again with `-v` to see what failed"]
        ),
        Code::PermissionDenied => explained!(
            "`mix` was not allowed to use a file or directory.",
            "Usually something under your home directory is owned by another user, often root after a command was run with sudo.",
            [
                "check who owns the path named in the message",
                "make it yours again with `chown`",
                "run the command again"
            ]
        ),
        Code::CommandFailed => explained!(
            "A program `mix` ran, such as `nix` or `git`, exited with an error.",
            "The program's own output says why, and `mix` keeps it under `Caused by`.",
            ["run the command again with `-v` to see the program's own output"]
        ),
        Code::SpawnFailed => explained!(
            "`mix` could not start a program it needs, such as `nix`.",
            "The program is missing, or it is not executable.",
            ["run `mix doctor` to see whether the runtime is installed"]
        ),
        Code::LockMissing => explained!(
            "`mix` has not been set up on this machine.",
            "The lock file `mix` uses to keep commands apart does not exist yet.",
            ["run `mix bootstrap`"]
        ),
        Code::Conflict => explained!(
            "A file or account changed while `mix` was working on it.",
            "`mix` left it as it found it instead of overwriting someone else's change.",
            ["run the command again to start from what is there now"]
        ),
        Code::UnitFailed => explained!(
            "A unit `mix` manages could not be started, stopped, enabled or reloaded.",
            "The unit is usually `nix-daemon`, and the message names it. Its own log says why it failed.",
            [
                "see why with `journalctl -u <unit>`",
                "run the command again"
            ]
        ),
        Code::SystemdUnreachable => explained!(
            "`mix` could not reach systemd.",
            "`mix` talks to systemd over the system bus, and nothing answered there.",
            [
                "check that D-Bus is running with `systemctl status dbus`",
                "run the command again"
            ]
        ),
        Code::JournalUnwritable => explained!(
            "`mix` could not record its progress.",
            "`mix` writes down each change before making it, so an interrupted command can be finished or undone later. Writing that record failed, usually because `/var/lib/mix` is full or read-only. The command itself went on, but if it is interrupted before it ends, `mix repair` may not be able to finish it.",
            ["make room in `/var/lib/mix`, or make it writable"]
        ),
        Code::CleanupIncomplete => explained!(
            "`mix` could not clean up after a step that did not finish.",
            "Something `mix` set up for the step could not be removed afterwards.",
            [
                "run `mix doctor` to see what is left",
                "run `mix repair` to put back what it can"
            ]
        ),
        Code::Network => explained!(
            "A download failed.",
            "The Nix runtime or a package could not be fetched.",
            [
                "check your internet connection, or the mirror given with `--mirror`",
                "run the command again"
            ]
        ),
        Code::Integrity => explained!(
            "A downloaded file did not match the checksum `mix` has pinned for it.",
            "It was damaged in transit, or the mirror serves the wrong file. It was thrown away unused.",
            ["run the command again to download it again"]
        ),
        Code::UnsupportedTarget => explained!(
            "`mix` does not support this system's architecture or operating system.",
            "`mix` runs on 64-bit Intel, AMD and ARM Linux.",
            []
        ),
        Code::Decompression => explained!(
            "A downloaded archive could not be unpacked.",
            "It was probably damaged in transit.",
            ["run the command again to download it again"]
        ),
        Code::MalformedArchive => explained!(
            "A downloaded archive is not laid out the way the Nix runtime is.",
            "The archive unpacked, but its contents are not what `mix` installs. This usually means the mirror serves a different file.",
            ["check that the mirror given with `--mirror` serves the official Nix release"]
        ),
        Code::NotRoot => explained!(
            "Setting up `mix` needs administrator rights.",
            "Setting up changes the whole system: it creates `/nix`, build users and a system service.",
            ["run `mix bootstrap` with sudo"]
        ),
        Code::UnsupportedHost => explained!(
            "This system is NixOS.",
            "NixOS manages packages with Nix natively. `mix` exists to bring that to other distributions, so it has nothing to do here.",
            []
        ),
        Code::UnsupportedKernel => explained!(
            "This system is WSL 1, and `mix` needs WSL 2.",
            "Nix builds packages in a sandbox that needs a real Linux kernel. WSL 1 translates system calls instead of running one, so it cannot host Nix.",
            ["upgrade the distro from Windows PowerShell with `wsl --set-version <distro> 2`"]
        ),
        Code::SystemdNotReady => explained!(
            "This system does not run systemd as its init system.",
            "`mix` runs the Nix daemon as a systemd service.",
            [
                "turn systemd on: on WSL, add `systemd=true` under `[boot]` in `/etc/wsl.conf` and restart the distro",
                "run the command again"
            ]
        ),
        Code::AlreadyManaged => explained!(
            "Nix is already installed here by something other than `mix`.",
            "`mix` needs to own its installation. Uninstalling the existing one removes everything installed with it.",
            ["uninstall the existing Nix", "run the command again"]
        ),
        Code::CrossDeviceStore => explained!(
            "`/nix/store` is a separate mount from `/nix`.",
            "`mix` moves the runtime into the store by renaming, which cannot cross filesystems.",
            [
                "remove the separate mount for `/nix/store`",
                "run the command again"
            ]
        ),
        Code::RollbackIncomplete => explained!(
            "The command failed, and undoing its changes failed too.",
            "Some of the changes the command had already made are still in place.",
            [
                "run `mix doctor` to see what is left",
                "run `mix repair` to put back what it can"
            ]
        ),
        Code::InvalidMirror => explained!(
            "The mirror settings are not usable.",
            "`--mirror` must be an http or https URL, and `--mirror-key` a single `<name>:<key>` entry.",
            ["pass the settings in that form", "run the command again"]
        ),
        Code::UnsupportedProgram => explained!(
            "A program `mix` runs is older than the oldest version it supports.",
            "`mix` relies on behavior that older releases of the program do not have, so it stops instead of guessing.",
            [
                "install a newer version, or add one to your Nix profile",
                "run the command again"
            ]
        ),
        Code::GitRecordFailed => explained!(
            "The change was made, but recording it in git failed.",
            "`mix` keeps your package list in a git repository so every change can be seen and undone. Committing fails most often because `git` is missing or the repository belongs to another user.",
            ["run `mix repair` to record it"]
        ),
        Code::UnknownPackage => explained!(
            "The package list names a package that does not exist.",
            "The package collection `mix` installs from has nothing by that name. Usually the name is misspelt, or the package goes by another name.",
            ["check the name", "run the command again"]
        ),
        Code::BuildFailed => explained!(
            "A package could not be built from source, and nothing was changed.",
            "The build's own output is under `Caused by`. A package that fails to build here usually fails for everyone.",
            [
                "run the command again with `-vv` to see the whole build",
                "use a different version or package"
            ]
        ),
        Code::NotBootstrapped => explained!(
            "`mix` has not been set up for your user on this machine.",
            "Each user needs their own setup before `mix` can manage their packages.",
            ["run `mix bootstrap`"]
        ),
        Code::RootNotAllowed => explained!(
            "Installing and removing packages cannot run as root.",
            "These commands change your own profile, so they run as you.",
            ["run the command again without sudo"]
        ),
        Code::InvalidPackage => explained!(
            "A package name is not a plain Nix identifier.",
            "A name such as `ripgrep` or `python3` cannot change the configuration it is written into, and anything else could.",
            ["use the package's plain name"]
        ),
        Code::InvalidState => explained!(
            "The package list `mix` would write is not valid.",
            "`mix` checks what it writes, and this list failed the check. This is a bug in `mix`.",
            [
                "report it at https://github.com/recregt/mix/issues with the output of the command run with `-v`"
            ]
        ),
        Code::NewerState => explained!(
            "Your package list was written by a newer version of `mix`.",
            "This version leaves it alone instead of misreading it.",
            ["update `mix`"]
        ),
        Code::ProtectedPackage => explained!(
            "`mix` needs this package to work.",
            "Some packages, such as `git`, which records every change to your configuration, cannot be removed.",
            []
        ),
        Code::Unrepairable => explained!(
            "`mix repair` found something it will not change on its own.",
            "It is something in the way that it would have to delete, a user that no longer exists, or a missing Nix runtime. The message says which.",
            ["use the help under the message"]
        ),
        Code::PrivilegesUnavailable => explained!(
            "`mix` could not get administrator rights.",
            "The command needs them, and `mix` asks for them through sudo. sudo could not be started, or it refused.",
            [
                "make sure your account can use sudo",
                "run the command again"
            ]
        ),
        Code::WorkerEnded => explained!(
            "The privileged helper stopped before it answered.",
            "`mix` starts a helper with administrator rights for this command. Whatever it had changed is undone by the next command.",
            ["run the command again to finish"]
        ),
        Code::Usage => explained!(
            "The command line was not one `mix` understands.",
            "A subcommand, flag or value was misspelled, missing, or given where it does not belong.",
            ["run `mix help` to see the commands and their flags"]
        ),
        Code::DaemonOutdated => explained!(
            "The running `mix` daemon is older than the `mix` that sent the request.",
            "The daemon does the work for every command after `mix bootstrap`, and it was installed by an earlier `mix`. A request can carry settings an older daemon does not know, such as `--dry-run`, and an older daemon would carry the request out without them. So `mix` refused to send it, and nothing was changed.",
            ["run `mix bootstrap` to install the daemon that matches this `mix`"]
        ),
        Code::VersionMismatch => explained!(
            "The privileged helper is a different version of `mix` than the command that started it.",
            "`mix` starts its helper with administrator rights from its own program file. That file was replaced, usually by an update, between the command starting and the helper starting. The two must be the same version, so the helper refused the request before changing anything.",
            ["run the command again"]
        ),
    }
}

pub fn explanation_text(code: Code) -> String {
    let explanation = explanation(code);
    let mut out = format!(
        "{}\n\n{}\n\n{}",
        name(code),
        explanation.description,
        explanation.why
    );
    if !explanation.fix.is_empty() {
        out.push_str("\n\nTo fix it:");
        for step in explanation.fix {
            out.push_str("\n  - ");
            out.push_str(step);
        }
    }
    out
}

#[cfg(test)]
mod tests;
