use crate::error::{Error, Result};

pub enum EscalationOutcome {
    ReExecuted { exit_code: i32 },
}

pub fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
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
    let current_exe = std::env::current_exe().map_err(|e| Error::Io {
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

    let status = command.status().map_err(|e| Error::Exec {
        command: "sudo".into(),
        source: e,
    })?;

    Ok(EscalationOutcome::ReExecuted {
        exit_code: status.code().unwrap_or(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn allowed_env_vars_preserves_path_for_sudo_command_lookup() {
        let mut env = std::collections::HashMap::new();
        env.insert(
            "PATH".to_string(),
            "/usr/local/sbin:/usr/sbin:/usr/bin".to_string(),
        );

        let result = allowed_env_vars(|key| env.get(key).cloned());

        assert_eq!(
            result,
            vec![("PATH", "/usr/local/sbin:/usr/sbin:/usr/bin".to_string())]
        );
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
