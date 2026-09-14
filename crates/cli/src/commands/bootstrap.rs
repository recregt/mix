use std::process::ExitCode;

use super::RootStatus;
use crate::ui;

pub async fn run(mirror: Option<String>) -> anyhow::Result<ExitCode> {
    if let RootStatus::ReExecuted(code) = super::ensure_root()? {
        return Ok(code);
    }

    mix_app::bootstrap::bootstrap(mirror.as_deref()).await?;
    ui::ok("System environment initialized and ready.");
    Ok(ExitCode::SUCCESS)
}
