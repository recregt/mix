use mix_events::v1::Code;

pub fn parse(name: &str) -> Option<Code> {
    let upper = name.trim().to_ascii_uppercase();
    let full = if upper.starts_with("CODE_") {
        upper
    } else {
        format!("CODE_{upper}")
    };
    Code::from_str_name(&full).filter(|code| *code != Code::Unspecified)
}

pub fn name(code: Code) -> &'static str {
    code.as_str_name().trim_start_matches("CODE_")
}

pub fn long(code: Code) -> &'static str {
    match code {
        Code::Unspecified => "No code was given. A failure without a code is a bug in `mix`.",
        Code::Internal => {
            "Something went wrong inside `mix` itself: a task panicked, or two parts of `mix` \
             disagreed about the protocol between them. Nothing on your system caused it. \
             Report it at https://github.com/recregt/mix/issues with the output of the command \
             run again with `-v`."
        }
        Code::Io => {
            "Reading or writing a file failed for a reason other than permissions: a missing \
             directory, a full disk, a read-only filesystem. Run the command again with `-v` to \
             see which path and which error."
        }
        Code::PermissionDenied => {
            "`mix` was not allowed to use a file or directory. Usually something under your home \
             directory is owned by another user, often root after a command was run with sudo. \
             Check who owns the path named in the message, give it back to yourself, and run the \
             command again."
        }
        Code::CommandFailed => {
            "A program `mix` ran, such as `nix` or `git`, exited with an error. Run the command \
             again with `-v` to see that program's own output."
        }
        Code::SpawnFailed => {
            "`mix` could not start a program it needs, such as `nix`. It may be missing, or not \
             executable. `mix doctor` shows whether the runtime is installed."
        }
        Code::Locked => {
            "Another `mix` command is already changing the system, and two changes at once could \
             leave it half done. Wait for the other command to finish, then run yours again."
        }
        Code::LockMissing => {
            "The lock file `mix` uses to keep commands apart does not exist, so `mix` has not \
             been set up on this machine. Run `mix bootstrap` first."
        }
        Code::Conflict => {
            "A file or account changed while `mix` was working on it, so `mix` left it as it \
             found it instead of overwriting someone else's change. Run the command again: it \
             starts from what is there now."
        }
        Code::UnitFailed => {
            "systemd could not start, stop, enable or reload one of the units `mix` manages, \
             usually `nix-daemon`. The message names the unit; `journalctl -u <unit>` shows why."
        }
        Code::SystemdUnreachable => {
            "`mix` talks to systemd over the system bus, and nothing answered there. Check that \
             D-Bus is running with `systemctl status dbus`."
        }
        Code::Network => {
            "A download failed: the Nix runtime or a package could not be fetched. Check your \
             internet connection, or the mirror given with `--mirror`, and run the command again."
        }
        Code::Integrity => {
            "A downloaded file did not match the checksum `mix` has pinned for it, so it was \
             thrown away unused. It was damaged in transit, or the mirror serves the wrong file. \
             Running the command again downloads it again."
        }
        Code::UnsupportedTarget => {
            "`mix` runs on 64-bit Intel, AMD and ARM Linux. This system's architecture or \
             operating system is not one of them."
        }
        Code::Decompression => {
            "A downloaded archive could not be unpacked. It was probably damaged in transit; \
             running the command again downloads it again."
        }
        Code::MalformedArchive => {
            "A downloaded archive unpacked, but its contents are not laid out the way the Nix \
             runtime is. If you use `--mirror`, check that it serves the official Nix release."
        }
        Code::NotRoot => {
            "Setting up `mix` changes the whole system: it creates `/nix`, build users and a \
             system service. Run `mix bootstrap` with sudo."
        }
        Code::UnsupportedHost => {
            "This system is NixOS, which manages packages with Nix natively. `mix` exists to \
             bring that to other distributions, so it has nothing to do here."
        }
        Code::UnsupportedKernel => {
            "Nix builds packages in a sandbox that needs a real Linux kernel. WSL 1 translates \
             system calls instead of running one, so it cannot host Nix. Convert the distro to \
             WSL 2 with `wsl --set-version <distro> 2`."
        }
        Code::SystemdNotReady => {
            "`mix` runs the Nix daemon as a systemd service, and systemd is not running as the \
             init system. On WSL, enable it with `systemd=true` under `[boot]` in \
             `/etc/wsl.conf`, then restart the distro."
        }
        Code::AlreadyManaged => {
            "Nix is already installed on this system, set up by something other than `mix`. \
             `mix` needs to own its installation, so uninstall the existing one first. That \
             removes everything installed with it."
        }
        Code::CrossDeviceStore => {
            "`/nix/store` is a separate mount from `/nix`, and `mix` moves the runtime into the \
             store by renaming, which cannot cross filesystems. Remove the separate mount."
        }
        Code::RollbackIncomplete => {
            "The command failed, and undoing what it had already changed failed too, so some \
             changes are still in place. `mix doctor` lists what is left; `mix repair` puts back \
             what it can."
        }
        Code::InvalidMirror => {
            "The mirror settings are not usable: `--mirror` must be an http or https URL, and \
             `--mirror-key` a single `<name>:<key>` entry."
        }
        Code::NotBootstrapped => {
            "`mix` has not been set up for your user on this machine. Run `mix bootstrap` \
             first."
        }
        Code::RootNotAllowed => {
            "Installing and removing packages changes your own profile, so it runs as you, not \
             as root. Run the command again without sudo."
        }
        Code::InvalidPackage => {
            "A package name has to be a plain Nix identifier, such as `ripgrep` or `python3`, so \
             that it cannot change the configuration it is written into."
        }
        Code::InvalidState => {
            "The package list `mix` would write is not valid. This is a bug in `mix`; report it \
             at https://github.com/recregt/mix/issues."
        }
        Code::NewerState => {
            "Your package list was written by a newer version of `mix` than this one, so this \
             version leaves it alone instead of misreading it. Update `mix`."
        }
        Code::ProtectedPackage => {
            "`mix` needs some packages to work, such as `git`, which records every change to \
             your configuration. They cannot be removed."
        }
        Code::Unrepairable => {
            "`mix repair` found something it will not change on its own: something in the way it \
             would have to delete, a user that no longer exists, or a missing Nix runtime. The \
             message says what to do about it."
        }
        Code::PrivilegesUnavailable => {
            "The command needs administrator rights, and `mix` asks for them through sudo. sudo \
             could not be started, or refused. Make sure your account can use sudo."
        }
        Code::WorkerEnded => {
            "The privileged helper `mix` started for this command stopped before it answered. \
             Whatever it had changed is undone by the next command; run the same command again \
             to finish."
        }
    }
}

#[cfg(test)]
mod tests;
