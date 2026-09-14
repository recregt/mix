use std::process::ExitCode;

use super::RootStatus;

pub async fn run(mirror: Option<String>) -> anyhow::Result<ExitCode> {
    if let RootStatus::ReExecuted(code) = super::ensure_root()? {
        return Ok(code);
    }
    let _lock = super::acquire_lock()?;

    mix_app::bootstrap::bootstrap(mirror.as_deref()).await?;
    mix_ui::ok("System environment initialized and ready.");
    Ok(ExitCode::SUCCESS)
}
