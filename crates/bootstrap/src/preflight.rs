use std::path::Path;

use mix_core::{Error, Result};

use crate::detect::{self, Wsl};

pub fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

pub fn ensure_root(reason: &'static str) -> Result<()> {
    if is_root() {
        return Ok(());
    }

    eprintln!("mix needs root to {reason}; re-running via `sudo`...");

    let current_exe = std::env::current_exe().map_err(|e| Error::Io {
        path: "/proc/self/exe".into(),
        source: e,
    })?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    const ENV_ALLOWLIST: &[&str] = &["RUST_LOG", "RUST_BACKTRACE", "NO_COLOR"];
    let mut command = std::process::Command::new("sudo");
    command.arg("--set-home").arg(&current_exe).args(&args);
    command.env_clear();
    for key in ENV_ALLOWLIST {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }

    let status = command.status().map_err(|e| Error::Command {
        command: "sudo".into(),
        detail: e.to_string(),
    })?;

    std::process::exit(status.code().unwrap_or(1));
}

pub fn check_not_nixos() -> Result<()> {
    if Path::new("/etc/NIXOS").exists() {
        return Err(Error::Other(
            "this looks like NixOS already -- mix's bootstrap is for non-NixOS Linux/WSL2".into(),
        ));
    }
    Ok(())
}

pub async fn check_nix_not_installed() -> Result<()> {
    let already = tokio::process::Command::new("nix-env")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .status()
        .await
        .is_ok();

    if already {
        return Err(Error::Other(
            "Nix already appears to be installed (`nix-env --version` succeeded)".into(),
        ));
    }
    Ok(())
}

pub fn check_not_wsl1() -> Result<()> {
    if detect::wsl::detect() == Wsl::V1 {
        return Err(Error::Other(
            "WSL1 detected -- Nix's sandbox needs a real Linux kernel, which WSL1 doesn't \
             provide. Upgrade the distro to WSL2 (`wsl --set-version <distro> 2`) and retry."
                .into(),
        ));
    }
    Ok(())
}

pub fn check_systemd_ready() -> Result<()> {
    if detect::wsl::systemd_active() {
        return Ok(());
    }

    let hint = if detect::wsl::detect() != Wsl::No {
        "On WSL2, systemd isn't enabled by default. Add `[boot]\\nsystemd=true` to \
         `/etc/wsl.conf`, then run `wsl.exe --shutdown` from Windows and reopen the \
         distro."
    } else {
        "systemd doesn't appear to be active (`/run/systemd/system` missing or PID 1 \
         isn't systemd)."
    };

    Err(Error::Other(format!(
        "{hint} mix's default install needs systemd to run the Nix daemon."
    )))
}
