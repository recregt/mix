pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod remove;
pub mod repair;

use mix_core::models::UserConfig;
use mix_shell::effect::lock::LockGuard;
use mix_shell::profile::change::Error;

fn exclusive_lock() -> mix_core::Result<LockGuard> {
    mix_shell::effect::lock::acquire_exclusive(mix_core::paths::LOCK_FILE)
}

pub fn enrolled_user() -> Option<UserConfig> {
    mix_shell::effect::accounts::invoking_user()
        .and_then(mix_shell::profile::existing_user_config_for)
}

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

pub fn acquire_lock() -> anyhow::Result<LockGuard> {
    Ok(exclusive_lock()?)
}

pub fn acquire_profile() -> Result<(LockGuard, UserConfig), Error> {
    if mix_shell::effect::accounts::is_root() {
        return Err(Error::NotRoot);
    }
    let lock = exclusive_lock()?;

    let Some(user_config) = enrolled_user() else {
        return Err(Error::NotBootstrapped);
    };

    Ok((lock, user_config))
}

pub(crate) fn human(
    reporters: crate::render::human::Reporters,
    json: bool,
) -> crate::render::human::Human {
    let human = crate::render::human::Human::new(reporters);
    if json { human.without_results() } else { human }
}
