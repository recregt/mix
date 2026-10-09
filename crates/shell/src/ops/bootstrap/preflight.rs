use std::path::Path;

use mix_core::declared::paths::{NIX_OWNERSHIP_MARKER, NIXOS_MARKER};
use mix_exec::Scope;

use crate::effect::fs::exists;
use crate::effect::fs::{is_dir, is_file};
use crate::ops::bootstrap::detect::{self, Wsl};
use crate::ops::bootstrap::error::{Error, Host, Result};

pub async fn check(force: bool, scope: &Scope) -> Result<()> {
    check_not_nixos().await?;
    check_not_wsl1().await?;
    check_systemd_ready().await?;
    if !force {
        check_nix_not_installed(scope).await?;
    }
    Ok(())
}

pub async fn check_not_nixos() -> Result<()> {
    check_not_nixos_at(Path::new(NIXOS_MARKER)).await
}

async fn check_not_nixos_at(marker: &Path) -> Result<()> {
    if exists(marker).await {
        return Err(Error::UnsupportedHost);
    }
    Ok(())
}

pub async fn check_nix_not_installed(scope: &Scope) -> Result<()> {
    if is_file(NIX_OWNERSHIP_MARKER).await {
        return Ok(());
    }

    let on_path = mix_exec::Command::new("nix-env")
        .arg("--version")
        .output(scope)
        .await
        .is_ok();

    if on_path || is_dir("/nix/store").await {
        return Err(Error::AlreadyManaged);
    }
    Ok(())
}

pub async fn check_not_wsl1() -> Result<()> {
    if detect::wsl::detect().await == Wsl::V1 {
        return Err(Error::UnsupportedKernel);
    }
    Ok(())
}

pub async fn check_systemd_ready() -> Result<()> {
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
#[allow(clippy::disallowed_methods)]
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
