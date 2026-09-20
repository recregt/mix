pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod repair;

use std::process::ExitCode;

use mix_app::profile::change::Error;
use mix_core::lock::LockGuard;
use mix_core::models::UserConfig;

pub enum RootStatus {
    AlreadyRoot,
    ReExecuted(ExitCode),
}

pub fn ensure_root() -> anyhow::Result<RootStatus> {
    if mix_core::privilege::is_root() {
        return Ok(RootStatus::AlreadyRoot);
    }

    mix_ui::info("Root required. Re-running with sudo...");

    let mix_core::privilege::EscalationOutcome::ReExecuted { exit_code } =
        mix_core::privilege::escalate()?;

    Ok(RootStatus::ReExecuted(ExitCode::from(
        exit_code.clamp(0, 255) as u8,
    )))
}

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
