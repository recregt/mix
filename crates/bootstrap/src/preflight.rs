use std::path::Path;

use mix_core::{Error, Result};

use crate::detect::{self, Wsl};

pub enum EscalationOutcome {
    ReExecuted { exit_code: i32 },
}

pub fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

pub fn escalate() -> Result<EscalationOutcome> {
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

    Ok(EscalationOutcome::ReExecuted {
        exit_code: status.code().unwrap_or(1),
    })
}

pub fn check_not_nixos() -> Result<()> {
    if Path::new("/etc/NIXOS").exists() {
        return Err(Error::UnsupportedHost);
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
        return Err(Error::AlreadyManaged);
    }
    Ok(())
}

pub fn check_not_wsl1() -> Result<()> {
    if detect::wsl::detect() == Wsl::V1 {
        return Err(Error::UnsupportedKernel);
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

    Err(Error::SystemdNotReady { hint })
}
