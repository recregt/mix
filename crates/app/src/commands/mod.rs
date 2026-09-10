pub mod bootstrap;
pub mod doctor;

use crate::ui;

pub fn ensure_root_or_exit(reason: &str) {
    if mix_bootstrap::preflight::is_root() {
        return;
    }

    ui::info(format!(
        "mix requires administrator privileges to {reason}. Re-executing with sudo..."
    ));

    match mix_bootstrap::preflight::escalate() {
        Ok(mix_bootstrap::preflight::EscalationOutcome::ReExecuted { exit_code }) => {
            std::process::exit(exit_code)
        }
        Err(e) => {
            ui::fail(e);
            std::process::exit(1);
        }
    }
}
