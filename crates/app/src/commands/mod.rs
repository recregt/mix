pub mod bootstrap;
pub mod doctor;

use std::process::ExitCode;

use crate::ui;

pub enum RootStatus {
    AlreadyRoot,
    ReExecuted(ExitCode),
}

pub fn ensure_root() -> anyhow::Result<RootStatus> {
    if mix_bootstrap::preflight::is_root() {
        return Ok(RootStatus::AlreadyRoot);
    }

    ui::info("Root required. Re-running with sudo...");

    let mix_bootstrap::preflight::EscalationOutcome::ReExecuted { exit_code } =
        mix_bootstrap::preflight::escalate()?;

    Ok(RootStatus::ReExecuted(ExitCode::from(
        exit_code.clamp(0, 255) as u8,
    )))
}
