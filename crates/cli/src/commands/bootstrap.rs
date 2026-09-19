use std::process::ExitCode;

use super::RootStatus;

pub async fn run(
    mirror: Option<String>,
    mirror_key: Option<String>,
    force: bool,
) -> anyhow::Result<ExitCode> {
    if let RootStatus::ReExecuted(code) = super::ensure_root()? {
        return Ok(code);
    }
    let _lock = super::acquire_lock()?;

    mix_app::bootstrap::bootstrap(
        mirror.as_deref(),
        mirror_key.as_deref(),
        force,
        mix_ui::download_reporter(),
        mix_ui::step_observer(),
        mix_ui::activity_reporter(),
    )
    .await?;
    mix_ui::ok("System environment initialized and ready.");
    Ok(ExitCode::SUCCESS)
}
