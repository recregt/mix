# Security policy

`mix` runs privileged code on the machines it manages, so security reports are welcome and taken seriously.

## Reporting a vulnerability

Do not open a public issue or pull request for a vulnerability.

Use GitHub's private reporting: open the [Security tab](https://github.com/recregt/mix/security) of the repository and choose **Report a vulnerability**.

Include:

- the `mix` version (`mix --version`) and the distribution it ran on,
- what you did, what you expected and what happened,
- a proof of concept or the exact commands, if you have them.

You will get an acknowledgement. After that we will confirm the problem, work on a fix with you, and agree on a disclosure date before anything is published. Reporters are credited in the advisory unless they prefer otherwise.

## Supported versions

`mix` is in alpha. Only the latest release receives security fixes.

## What `mix` protects

Knowing the design helps decide what counts as a vulnerability.

- **Privilege separation.** Only `mix bootstrap` and `mix purge` ask for `sudo`. After setup, privileged work runs in `mix-daemon`, which systemd starts on demand from a socket. `mix user remove` is one of those requests: the daemon removes the caller from `mix-users` and clears only the caller's own state, so leaving needs no administrator rights. Commands that work on packages refuse to run as root, and reads and writes inside a user's home happen as that user.
- **Pinned downloads.** The Nix runtime that `mix` downloads is checked against a checksum and size pinned in the source at build time. A mismatch stops the setup with the `integrity` failure code.
- **Mirrors.** A mirror is chosen by the administrator at bootstrap. Its signing key is trusted machine-wide, so only point `mix` at a mirror you control or trust.
- **Crash safety.** Every request is journaled. An interrupted request is rolled back or completed before the next one runs.
- **Reproducible builds.** Release binaries are built with Nix, and CI builds each one twice on several runners and compares the results.

## In scope

- Gaining root or another user's privileges through `mix`, `mix-daemon` or the socket at `/run/mix/daemon.sock`.
- Reading or writing files outside what a request is allowed to touch, including through symlinks, races or unusual path names.
- Using `mix user remove` to change another user's membership or state.
- Bypassing the checksum checks, or making `mix` trust a key or mirror the administrator did not choose.
- Making `mix` run code from a user's configuration as root.
- Corrupting another user's configuration or the shared Nix store.

## Out of scope

- Problems in Nix, nixpkgs, Home Manager or systemd themselves. Report those upstream. Tell us as well if `mix` makes them worse.
- Anything that already requires root on the machine.
- A malicious mirror that the administrator chose on purpose.
- Denial of service by a user against their own account.
- Reports about unsupported systems, such as NixOS or WSL 1.

## Dependencies

Dependencies are audited with `cargo audit` and `cargo deny` in CI, GitHub Actions workflows are checked with zizmor, and Renovate keeps both up to date. If you find a vulnerable dependency that these did not catch, report it the same way.
