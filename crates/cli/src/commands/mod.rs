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

pub fn acquire_lock() -> anyhow::Result<LockGuard> {
    Ok(mix_core::lock::acquire_exclusive(
        mix_core::paths::LOCK_FILE,
    )?)
}

pub fn acquire_profile() -> anyhow::Result<(LockGuard, UserConfig)> {
    if mix_core::privilege::is_root() {
        return Err(Error::NotRoot.into());
    }
    let lock = acquire_lock()?;

    let Some(user_config) = mix_app::resolve_existing_user_config() else {
        return Err(Error::NotBootstrapped.into());
    };

    Ok((lock, user_config))
}
