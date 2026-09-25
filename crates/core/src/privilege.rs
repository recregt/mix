use crate::error::{Error, Result};

pub enum EscalationOutcome {
    ReExecuted { exit_code: i32 },
}

pub fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokingUser {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    pub home: std::path::PathBuf,
}

pub fn invoking_user() -> Option<InvokingUser> {
    resolve_invoking_user(is_root(), nix::unistd::Uid::current(), |key| {
        std::env::var(key).ok()
    })
}

fn resolve_invoking_user(
    is_root: bool,
    current: nix::unistd::Uid,
    get: impl Fn(&str) -> Option<String>,
) -> Option<InvokingUser> {
    if is_root {
        invoking_user_from(get)
    } else {
        current_user(current)
    }
}

fn invoking_user_from(get: impl Fn(&str) -> Option<String>) -> Option<InvokingUser> {
    let uid: u32 = get("SUDO_UID")?.parse().ok()?;
    current_user(nix::unistd::Uid::from_raw(uid))
}

fn current_user(uid: nix::unistd::Uid) -> Option<InvokingUser> {
    let user = nix::unistd::User::from_uid(uid).ok().flatten()?;
    Some(InvokingUser {
        uid: uid.as_raw(),
        gid: user.gid.as_raw(),
        name: user.name,
        home: user.dir,
    })
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
    "MIX_NIX_MIRROR_KEY",
    "WSL_DISTRO_NAME",
    "WSL_INTEROP",
];

fn allowed_env_vars(get: impl Fn(&str) -> Option<String>) -> Vec<(&'static str, String)> {
    ENV_ALLOWLIST
        .iter()
        .filter_map(|&key| get(key).map(|value| (key, value)))
        .collect()
}

const SUDO_LOOKUP_ONLY: &str = "PATH";

fn sudo_command(
    exe: &std::path::Path,
    args: &[String],
    allowed: &[(&'static str, String)],
) -> std::process::Command {
    let mut command = std::process::Command::new("sudo");
    command.arg("--set-home");

    let preserved: Vec<&str> = allowed
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| *key != SUDO_LOOKUP_ONLY)
        .collect();
    if !preserved.is_empty() {
        command.arg(format!("--preserve-env={}", preserved.join(",")));
    }

    command.arg(exe).args(args);
    command.env_clear();
    for (key, value) in allowed {
        command.env(key, value);
    }
    command
}

pub fn escalate() -> Result<EscalationOutcome> {
    let current_exe = std::env::current_exe().map_err(|e| Error::Io {
        path: "/proc/self/exe".into(),
        source: e,
    })?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let allowed = allowed_env_vars(|key| std::env::var(key).ok());

    let status = sudo_command(&current_exe, &args, &allowed)
        .status()
        .map_err(|e| Error::Exec {
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
    fn allowed_env_vars_includes_the_mirror_key() {
        let mut env = std::collections::HashMap::new();
        env.insert("MIX_NIX_MIRROR_KEY".to_string(), "mix-1:AAAA=".to_string());

        let result = allowed_env_vars(|key| env.get(key).cloned());

        assert_eq!(
            result,
            vec![("MIX_NIX_MIRROR_KEY", "mix-1:AAAA=".to_string())]
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
    fn invoking_user_from_resolves_a_known_uid() {
        let mut env = std::collections::HashMap::new();
        env.insert("SUDO_UID".to_string(), "0".to_string());

        let user = invoking_user_from(|key| env.get(key).cloned()).unwrap();

        assert_eq!(user.uid, 0);
        assert_eq!(user.gid, 0);
        assert_eq!(user.name, "root");
        assert!(user.home.is_absolute());
    }

    #[test]
    fn invoking_user_from_none_when_sudo_uid_is_unset() {
        assert!(invoking_user_from(|_| None).is_none());
    }

    #[test]
    fn invoking_user_from_none_when_sudo_uid_is_not_a_number() {
        let mut env = std::collections::HashMap::new();
        env.insert("SUDO_UID".to_string(), "not-a-uid".to_string());

        assert!(invoking_user_from(|key| env.get(key).cloned()).is_none());
    }

    #[test]
    fn invoking_user_resolves_the_current_process_when_not_root() {
        if is_root() {
            return;
        }
        let user = invoking_user().expect("the current uid should have a passwd entry");
        assert_eq!(user.uid, nix::unistd::Uid::current().as_raw());
    }

    #[test]
    fn resolve_invoking_user_none_for_bare_root_without_sudo_uid() {
        assert!(resolve_invoking_user(true, nix::unistd::Uid::current(), |_| None).is_none());
    }

    #[test]
    fn resolve_invoking_user_uses_sudo_uid_when_root() {
        let mut env = std::collections::HashMap::new();
        env.insert("SUDO_UID".to_string(), "0".to_string());

        let user = resolve_invoking_user(true, nix::unistd::Uid::from_raw(4_294_967_295), |key| {
            env.get(key).cloned()
        })
        .unwrap();

        assert_eq!(user.uid, 0);
        assert_eq!(user.name, "root");
    }

    #[test]
    fn resolve_invoking_user_ignores_sudo_uid_when_not_root() {
        let mut env = std::collections::HashMap::new();
        env.insert("SUDO_UID".to_string(), "4294967295".to_string());

        let user = resolve_invoking_user(false, nix::unistd::Uid::from_raw(0), |key| {
            env.get(key).cloned()
        })
        .unwrap();

        assert_eq!(user.uid, 0);
        assert_eq!(user.name, "root");
    }

    #[test]
    fn invoking_user_from_none_when_the_uid_has_no_passwd_entry() {
        let mut env = std::collections::HashMap::new();
        env.insert("SUDO_UID".to_string(), "4294967295".to_string());

        assert!(invoking_user_from(|key| env.get(key).cloned()).is_none());
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

    fn arguments(command: &std::process::Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn environment(command: &std::process::Command) -> Vec<(String, String)> {
        command
            .get_envs()
            .filter_map(|(key, value)| {
                Some((
                    key.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect()
    }

    #[test]
    fn sudo_is_told_to_keep_every_allowed_variable_that_is_set() {
        let allowed = vec![
            ("PATH", "/usr/bin".to_string()),
            ("HTTPS_PROXY", "http://proxy:8080".to_string()),
            ("MIX_NIX_MIRROR", "http://mirror.internal".to_string()),
        ];

        let command = sudo_command(
            std::path::Path::new("/usr/local/bin/mix"),
            &["bootstrap".to_string()],
            &allowed,
        );

        assert_eq!(
            arguments(&command),
            [
                "--set-home",
                "--preserve-env=HTTPS_PROXY,MIX_NIX_MIRROR",
                "/usr/local/bin/mix",
                "bootstrap"
            ]
        );
        let mut env = environment(&command);
        env.sort();
        assert_eq!(
            env,
            [
                ("HTTPS_PROXY".to_string(), "http://proxy:8080".to_string()),
                (
                    "MIX_NIX_MIRROR".to_string(),
                    "http://mirror.internal".to_string()
                ),
                ("PATH".to_string(), "/usr/bin".to_string()),
            ]
        );
    }

    #[test]
    fn the_callers_path_is_only_used_to_find_sudo() {
        let command = sudo_command(
            std::path::Path::new("/usr/local/bin/mix"),
            &[],
            &[("PATH", "/home/user/bin:/usr/bin".to_string())],
        );

        assert!(!arguments(&command).iter().any(|arg| arg.contains("PATH")));
        assert_eq!(arguments(&command), ["--set-home", "/usr/local/bin/mix"]);
    }

    #[test]
    fn nothing_else_from_the_callers_environment_reaches_sudo() {
        let command = sudo_command(std::path::Path::new("/usr/local/bin/mix"), &[], &[]);

        assert!(environment(&command).is_empty());
        assert_eq!(arguments(&command), ["--set-home", "/usr/local/bin/mix"]);
    }
}
