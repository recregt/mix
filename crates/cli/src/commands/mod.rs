pub mod bootstrap;
pub mod doctor;
pub mod repair;

use std::process::ExitCode;

use crate::ui;

pub enum RootStatus {
    AlreadyRoot,
    ReExecuted(ExitCode),
}

pub fn ensure_root() -> anyhow::Result<RootStatus> {
    if mix_app::bootstrap::preflight::is_root() {
        return Ok(RootStatus::AlreadyRoot);
    }

    ui::info("Root required. Re-running with sudo...");

    let mix_app::bootstrap::preflight::EscalationOutcome::ReExecuted { exit_code } =
        mix_app::bootstrap::preflight::escalate()?;

    Ok(RootStatus::ReExecuted(ExitCode::from(
        exit_code.clamp(0, 255) as u8,
    )))
}
