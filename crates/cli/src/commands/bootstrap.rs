use std::process::ExitCode;

use super::RootStatus;

pub async fn run(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
) -> anyhow::Result<ExitCode> {
    if let RootStatus::ReExecuted(code) = super::ensure_root()? {
        return Ok(code);
    }
    let _lock = super::acquire_lock()?;

    mix_app::bootstrap::bootstrap(
        mirror,
        mirror_key,
        force,
        mix_ui::download_reporter(),
        mix_ui::step_observer(),
        mix_ui::activity_reporter(),
    )
    .await?;
    mix_ui::ok("System environment initialized and ready.");
    Ok(ExitCode::SUCCESS)
}
