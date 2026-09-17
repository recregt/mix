use std::path::Path;

use mix_core::paths::NIX_OWNERSHIP_MARKER;

use crate::bootstrap::detect::{self, Wsl};
use crate::bootstrap::error::{Error, Host, Result};
use crate::fs::exists;
use crate::fs::{is_dir, is_file};

pub async fn check_not_nixos() -> Result<()> {
    tracing::debug!("checking host is not NixOS");
    check_not_nixos_at(Path::new("/etc/NIXOS")).await
}

async fn check_not_nixos_at(marker: &Path) -> Result<()> {
    if exists(marker).await {
        return Err(Error::UnsupportedHost);
    }
    Ok(())
}

pub async fn check_nix_not_installed() -> Result<()> {
    tracing::debug!("checking for a pre-existing, unmanaged Nix installation");
    if is_file(NIX_OWNERSHIP_MARKER).await {
        return Ok(());
    }

    let on_path = tokio::process::Command::new("nix-env")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok();

    if on_path || is_dir("/nix/store").await {
        return Err(Error::AlreadyManaged);
    }
    Ok(())
}

pub async fn check_not_wsl1() -> Result<()> {
    tracing::debug!("checking WSL version");
    if detect::wsl::detect().await == Wsl::V1 {
        return Err(Error::UnsupportedKernel);
    }
    Ok(())
}

pub async fn check_systemd_ready() -> Result<()> {
    tracing::debug!("checking systemd is ready");
    if detect::wsl::systemd_active().await {
        return Ok(());
    }

    // Which host this is decides how systemd is turned back on, so it travels with the error;
    // the sentence that says how is written where the command is known.
    let host = if detect::wsl::detect().await != Wsl::No {
        Host::Wsl
    } else {
        Host::Native
    };

    Err(Error::SystemdNotReady { host })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn check_not_nixos_ok_when_marker_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_not_nixos_at(&dir.path().join("NIXOS")).await.is_ok());
    }

    #[tokio::test]
    async fn check_not_nixos_fails_when_marker_present() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("NIXOS");
        std::fs::write(&marker, "").unwrap();
        assert!(matches!(
            check_not_nixos_at(&marker).await,
            Err(Error::UnsupportedHost)
        ));
    }
}
