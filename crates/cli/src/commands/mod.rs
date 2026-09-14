pub mod bootstrap;
pub mod doctor;
pub mod repair;

use std::process::ExitCode;

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

pub fn acquire_lock() -> anyhow::Result<mix_core::lock::LockGuard> {
    Ok(mix_core::lock::acquire_exclusive(
        mix_core::paths::LOCK_FILE,
    )?)
}
