pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod remove;
pub mod repair;

use mix_app::profile::change::Error;
use mix_core::lock::LockGuard;
use mix_core::models::UserConfig;

fn exclusive_lock() -> mix_core::Result<LockGuard> {
    mix_core::lock::acquire_exclusive(mix_core::paths::LOCK_FILE)
}

pub fn enrolled_user() -> Option<UserConfig> {
    mix_core::privilege::invoking_user().and_then(mix_app::profile::existing_user_config_for)
}

pub fn host_config() -> mix_app::HostConfig {
    mix_app::HostConfig {
        git_binary: std::env::var_os("MIX_GIT_PATH").map(std::path::PathBuf::from),
    }
}

pub fn request_env(mirror: Option<&str>, mirror_key: Option<&str>) -> mix_app::RequestEnv {
    mix_app::RequestEnv {
        mirror: mirror.map(str::to_string),
        mirror_key: mirror_key.map(str::to_string),
    }
}

pub fn acquire_lock() -> anyhow::Result<LockGuard> {
    Ok(exclusive_lock()?)
}

pub fn acquire_profile() -> Result<(LockGuard, UserConfig), Error> {
    if mix_core::privilege::is_root() {
        return Err(Error::NotRoot);
    }
    let lock = exclusive_lock()?;

    let Some(user_config) = enrolled_user() else {
        return Err(Error::NotBootstrapped);
    };

    Ok((lock, user_config))
}
