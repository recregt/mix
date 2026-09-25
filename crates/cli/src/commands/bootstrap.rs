use std::process::ExitCode;

use mix_core::paths::PROFILE_SNIPPET_DEST;

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
    mix_ui::ok("mix is ready!");
    mix_ui::info("");
    mix_ui::info(format!(
        "to use installed packages in this terminal session, run:\n  source {PROFILE_SNIPPET_DEST}"
    ));
    Ok(ExitCode::SUCCESS)
}
