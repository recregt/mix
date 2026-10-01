pub fn host_config() -> mix_shell::HostConfig {
    mix_shell::HostConfig {
        git_binary: std::env::var_os("MIX_GIT_PATH").map(std::path::PathBuf::from),
    }
}

pub fn policy() -> mix_core::policy::Policy {
    let stored = std::fs::read_to_string(mix_core::paths::POLICY_FILE).ok();
    mix_core::policy::Policy::load(stored.as_deref())
}

pub fn requested_policy(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
) -> Result<mix_core::policy::Policy, mix_shell::ops::bootstrap::Error> {
    mix_core::policy::Policy::new(mirror, mirror_key)
        .map_err(|invalid| mix_shell::ops::bootstrap::Error::InvalidMirror(invalid.to_string()))
}
