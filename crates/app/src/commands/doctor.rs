use std::process::ExitCode;

use super::RootStatus;
use crate::ui;

pub async fn run(fix: bool, mirror: Option<String>) -> anyhow::Result<ExitCode> {
    if fix {
        if let RootStatus::ReExecuted(code) = super::ensure_root()? {
            return Ok(code);
        }

        mix_bootstrap::doctor(mirror.as_deref()).await?;
        ui::ok("System state successfully restored to pristine condition.");
        return Ok(ExitCode::SUCCESS);
    }

    check().await
}

pub async fn check() -> anyhow::Result<ExitCode> {
    match mix_bootstrap::Environment::open().await {
        Ok(_) => {
            ui::ok("System health is intact.");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            ui::fail(check_failed_message(e));
            Ok(ExitCode::FAILURE)
        }
    }
}

pub fn check_failed_message(e: impl std::fmt::Display) -> String {
    format!(
        "System health check failed: {e}\n\nRun `mix doctor --fix` to reconcile configuration drift."
    )
}
