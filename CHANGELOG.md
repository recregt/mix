# Changelog

All notable changes to `mix` are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed

- A user outside `mix-users` can no longer slow down or block `mix` for other users by holding connections to the daemon open, and the daemon no longer stops when it runs out of file descriptors. ([#157](https://github.com/recregt/mix/pull/157))
- `mix doctor` reports, and `mix repair` fixes, a wrong owner or a mode that lets others write on every file and directory `mix` installs as root, including `/etc/nix/nix.conf`, `/var/lib/mix`, the systemd units and the request journals. ([#158](https://github.com/recregt/mix/pull/158))
- A file `mix` manages that nobody can read is reported as having the wrong permissions, and `mix repair` fixes it. Errors no longer show internal values such as `Some(98)` or a raw `CommandFailed { .. }`. ([#159](https://github.com/recregt/mix/pull/159))
- `mix doctor` reports a `~/.nix-profile` that is not a link, and `mix install` stops with that reason instead of failing inside Nix. ([#160](https://github.com/recregt/mix/pull/160))
- `mix doctor` names files in the way once, instead of "files in /home/alice files in the way are in the way". ([#161](https://github.com/recregt/mix/pull/161))
- After a `mix bootstrap` was interrupted, `mix-daemon` no longer hangs in "Recovering interrupted requests" and blocks `mix`, `systemctl stop` and boot. It undoes the bootstrap and stops. ([#163](https://github.com/recregt/mix/pull/163))
- When `mix-daemon` is stopped, keeps failing to start or is damaged, commands say "the `mix` daemon isn't answering" and point to `mix bootstrap`, instead of "isn't set up for you yet" or "couldn't get administrator rights". ([#164](https://github.com/recregt/mix/pull/164))
- After `mix` is upgraded or downgraded with the install script, commands say the daemon is a different version and point to `mix bootstrap`, instead of asking to run the same command again, and the install script ends with that step. ([#165](https://github.com/recregt/mix/pull/165))
- When `mix-daemon` stops partway through a command, the command says it stopped before it could finish, instead of exiting with nothing printed, and `mix doctor` reports a managed file above 4 MiB instead of failing silently. ([#167](https://github.com/recregt/mix/pull/167))
- On a terminal, `mix doctor` and `mix repair` cut a long changed line to the terminal width instead of printing it whole in the repair suggestion, and `mix list` and `mix explain` cut long descriptions the same way. Output that is not a terminal is unchanged. ([#169](https://github.com/recregt/mix/pull/169))
- `mix doctor` reports a package list written by a newer `mix` instead of calling the profile stale, and `mix repair` leaves it alone instead of building a new generation on every run. ([#170](https://github.com/recregt/mix/pull/170))
- A power cut while `mix` records a change no longer leaves empty files in the state repository, because every git write is synced to disk. ([#171](https://github.com/recregt/mix/pull/171))
- `mix repair` puts back a deleted profile and history in one run, and `mix` always uses the git in your profile instead of falling back to the system `git`. The `unsupported-program` failure code is gone. ([#172](https://github.com/recregt/mix/pull/172))
- A power cut shortly after `mix install` no longer leaves empty files in the Nix store or an interrupted request that can never be undone, because new store paths are synced to disk before they are used. ([#177](https://github.com/recregt/mix/pull/177))
- When the output can't be written, for example to a full disk, `mix` says so and exits 1 instead of exiting 0 with the output cut short. A reader that stops early, like `head`, still ends the output quietly. ([#178](https://github.com/recregt/mix/pull/178))

## [0.1.0-alpha.2] - 2026-10-19

### Added

- An install script: `curl -fsSL https://mix.recregt.com/install.sh | sh` downloads the newest release for your architecture, checks its checksum and installs `mix` and `mix-daemon` into `/usr/local/bin`. ([#137](https://github.com/recregt/mix/pull/137))
- `mix user remove` removes `mix` from your own account without `sudo`. When you are the last user, it offers to remove Nix from the machine as well. ([#127](https://github.com/recregt/mix/pull/127))
- `mix purge` removes `mix`, Nix and every user's `mix` state from the machine, including state left in the home of a former user. Its confirmation names each user, and `--dry-run` shows every change first. ([#127](https://github.com/recregt/mix/pull/127))

### Changed

- The `--json` output writes every field of a result, even at its default value, and names each change by its path. `cancellation` appears only when a run was cancelled. ([#133](https://github.com/recregt/mix/pull/133))
- A package that does not exist or fails to build is named in the failure itself, not only in the command that failed. ([#133](https://github.com/recregt/mix/pull/133))

### Removed

- `mix self remove`. Use `mix user remove` to leave, or `mix purge` to remove `mix` from the whole machine. ([#127](https://github.com/recregt/mix/pull/127))

### Fixed

- The first `mix list` after an install no longer downloads nixpkgs again and works offline, and `mix update` no longer downloads the nixpkgs you already have. ([#128](https://github.com/recregt/mix/pull/128))
- `mix user remove` and `mix purge` no longer fail and roll back when the daemon is listing packages in the background at the same time. ([#130](https://github.com/recregt/mix/pull/130), [#131](https://github.com/recregt/mix/pull/131))
- A dry run of `mix install`, `mix remove` or `mix update` lists the change that records the new package list, as the real run does. ([#133](https://github.com/recregt/mix/pull/133))

## [0.1.0-alpha.1] - 2026-10-17

The first public alpha.

### Added

- `mix bootstrap` sets up a multi-user Nix with pinned, checksummed downloads.
- `mix install`, `mix remove`, `mix list`, `mix update` and `mix clean` manage your own package list.
- `mix doctor` and `mix repair` check the machine and fix configuration drift.
- `mix explain` describes every failure code.
- `mix self remove` takes `mix`, Nix and every user's `mix` state off the machine.
- Interrupted requests are finished or undone the next time `mix` runs.

[Unreleased]: https://github.com/recregt/mix/compare/v0.1.0-alpha.2...HEAD
[0.1.0-alpha.2]: https://github.com/recregt/mix/compare/v0.1.0-alpha.1...v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/recregt/mix/releases/tag/v0.1.0-alpha.1
