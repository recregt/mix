use std::path::Path;

use crate::constants::NIX_OWNERSHIP_MARKER;
use crate::detect::{self, Wsl};
use crate::error::{Error, Result};
use crate::util::{is_dir, is_file, path_exists};

pub enum EscalationOutcome {
    ReExecuted { exit_code: i32 },
}

pub fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

const ENV_ALLOWLIST: &[&str] = &[
    "NO_COLOR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "MIX_NIX_MIRROR",
    "WSL_DISTRO_NAME",
    "WSL_INTEROP",
];

fn allowed_env_vars(get: impl Fn(&str) -> Option<String>) -> Vec<(&'static str, String)> {
    ENV_ALLOWLIST
        .iter()
        .filter_map(|&key| get(key).map(|value| (key, value)))
        .collect()
}

pub fn escalate() -> Result<EscalationOutcome> {
    let current_exe = std::env::current_exe().map_err(|e| mix_core::Error::Io {
        path: "/proc/self/exe".into(),
        source: e,
    })?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut command = std::process::Command::new("sudo");
    command.arg("--set-home").arg(&current_exe).args(&args);
    command.env_clear();
    for (key, value) in allowed_env_vars(|key| std::env::var(key).ok()) {
        command.env(key, value);
    }

    let status = command.status().map_err(|e| mix_core::Error::Exec {
        command: "sudo".into(),
        source: e,
    })?;

    Ok(EscalationOutcome::ReExecuted {
        exit_code: status.code().unwrap_or(1),
    })
}

pub async fn check_not_nixos() -> Result<()> {
    tracing::debug!("checking host is not NixOS");
    check_not_nixos_at(Path::new("/etc/NIXOS")).await
}

async fn check_not_nixos_at(marker: &Path) -> Result<()> {
    if path_exists(marker).await {
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

    let hint = if detect::wsl::detect().await != Wsl::No {
        "On WSL2, systemd isn't enabled by default. Add `[boot]\\nsystemd=true` to \
         `/etc/wsl.conf`, then run `wsl.exe --shutdown` from Windows and reopen the \
         distro."
    } else {
        "systemd doesn't appear to be active (`/run/systemd/system` missing or PID 1 \
         isn't systemd).\n\
         `mix` needs systemd to manage its background services; check that it is \
         installed and set as your init system, then retry."
    };

    Err(Error::SystemdNotReady { hint })
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

    #[test]
    fn allowed_env_vars_passes_through_known_keys_only() {
        let mut env = std::collections::HashMap::new();
        env.insert("HTTP_PROXY".to_string(), "http://proxy:8080".to_string());
        env.insert("SECRET_TOKEN".to_string(), "xyz".to_string());

        let result = allowed_env_vars(|key| env.get(key).cloned());

        assert_eq!(
            result,
            vec![("HTTP_PROXY", "http://proxy:8080".to_string())]
        );
    }

    #[test]
    fn allowed_env_vars_includes_the_nix_mirror() {
        let mut env = std::collections::HashMap::new();
        env.insert(
            "MIX_NIX_MIRROR".to_string(),
            "http://mirror.internal".to_string(),
        );

        let result = allowed_env_vars(|key| env.get(key).cloned());

        assert_eq!(
            result,
            vec![("MIX_NIX_MIRROR", "http://mirror.internal".to_string())]
        );
    }

    #[test]
    fn allowed_env_vars_empty_when_nothing_set() {
        assert!(allowed_env_vars(|_| None).is_empty());
    }

    #[test]
    fn allowed_env_vars_preserves_wsl_detection_vars() {
        let mut env = std::collections::HashMap::new();
        env.insert("WSL_DISTRO_NAME".to_string(), "Ubuntu".to_string());
        env.insert("WSL_INTEROP".to_string(), "/run/WSL/1_interop".to_string());

        let result = allowed_env_vars(|key| env.get(key).cloned());

        assert_eq!(
            result,
            vec![
                ("WSL_DISTRO_NAME", "Ubuntu".to_string()),
                ("WSL_INTEROP", "/run/WSL/1_interop".to_string()),
            ]
        );
    }
}
