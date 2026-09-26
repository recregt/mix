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

pub fn acquire_lock() -> anyhow::Result<LockGuard> {
    Ok(exclusive_lock()?)
}

pub fn acquire_profile() -> Result<(LockGuard, UserConfig), Error> {
    if mix_core::privilege::is_root() {
        return Err(Error::NotRoot);
    }
    let lock = exclusive_lock()?;

    let Some(user_config) = mix_app::resolve_existing_user_config() else {
        return Err(Error::NotBootstrapped);
    };

    Ok((lock, user_config))
}
