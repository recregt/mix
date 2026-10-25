# Changelog

All notable changes to `mix` are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- With `--json`, `mix install --dry-run` reports every new package in one run instead of stopping at the first bad name, with whether it can be installed, its version and licenses, and what the install would download or build. ([#196](https://github.com/recregt/mix/pull/196))

### Changed

- New setups install Nix 2.35.4 instead of 2.35.2, and `mix bootstrap` moves every user to nixpkgs and home-manager as of October 2026. ([#202](https://github.com/recregt/mix/pull/202))

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
- When the disk that holds the Nix store is full, commands say so and suggest `mix clean --all`, instead of ending with "1 dependency failed". ([#179](https://github.com/recregt/mix/pull/179))
- `mix bootstrap --force` replaces a Nix from any installer, and when it fails it puts that Nix back as it was, with its units enabled and running, instead of leaving neither Nix nor `mix` working. A `mix purge` that fails also puts every unit back enabled. ([#180](https://github.com/recregt/mix/pull/180))
- After `mix bootstrap --force`, `mix doctor` no longer reports old Home Manager generations whose store paths are gone, because the old profile links are moved aside with the old Nix. A forced bootstrap or purge also works when `/nix` holds the daemon's socket, and a purge finds unit links even after the unit file is gone. ([#182](https://github.com/recregt/mix/pull/182))
- `mix update` no longer fails with GitHub's API rate limit, because it finds the newest nixpkgs and home-manager with `git` and fetches that exact revision. A failed download says which URL it could not fetch and suggests checking the connection, instead of showing Nix's raw error. ([#184](https://github.com/recregt/mix/pull/184))
- While an interrupted request waits to be finished or undone, `mix doctor` says so and that the next change or `mix repair` settles it, instead of saying it couldn't be put back and that `mix repair` can't fix it. The files it still holds are shown as waiting on it, instead of "changed outside `mix`". ([#185](https://github.com/recregt/mix/pull/185))
- `mix install --dry-run` checks with Nix that each new package exists and fails the way the install would for one it does not know, instead of ending with "nothing was changed". The check updates only Nix's own cache in `~/.cache/nix`. ([#196](https://github.com/recregt/mix/pull/196))
- When the disk that holds the Nix store runs out of inodes, commands say so and point to `df -i`, instead of saying the disk is full while `df -h` shows free space. ([#197](https://github.com/recregt/mix/pull/197))
- When two packages provide the same file, such as two versions of Lua, commands name both packages and the file, instead of "couldn't build home-manager-generation". ([#198](https://github.com/recregt/mix/pull/198))
- `mix purge` and `mix user remove` say "Removing mix from /data/home" for the step that removes a user's `mix` files, instead of "Removing /data/home", which read as if the whole home would be deleted. ([#199](https://github.com/recregt/mix/pull/199))
- `mix purge` removes `/run/mix` and the daemon's socket in it, instead of leaving them until the next reboot. ([#200](https://github.com/recregt/mix/pull/200))
- When the machine runs out of memory during an install, the command says the build ran out of memory and exits 1, instead of saying it was cancelled and exiting 130. The daemon keeps running instead of being stopped by systemd. ([#201](https://github.com/recregt/mix/pull/201))
- A long Nix error, such as the one for a package nixpkgs marks insecure, is shown as text instead of a raw `@nix` record cut in two. ([#214](https://github.com/recregt/mix/pull/214))
- A Ctrl-C that comes too late to cancel, while `mix` records a finished change, no longer says it is putting the package list back. ([#215](https://github.com/recregt/mix/pull/215))
- A failure no longer lists Nix's "Git tree is dirty" warning as its cause. ([#216](https://github.com/recregt/mix/pull/216))
- `mix user remove` says "recregt still uses `mix`" when one other user is left, instead of "still use". ([#217](https://github.com/recregt/mix/pull/217))
- `mix doctor` says a changed unit file "differs from what `mix` wrote", instead of calling the `mix-daemon` units the Nix runtime's copy. ([#218](https://github.com/recregt/mix/pull/218))

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
