use std::process::ExitCode;

use mix_app::remove::Error;

pub async fn run(
    packages: Vec<String>,
    mirror: Option<String>,
    mirror_key: Option<String>,
    json: bool,
) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile().map_err(Error::from)?;

    let removed = mix_app::remove::remove(
        &user_config,
        &packages,
        mirror.as_deref(),
        mirror_key.as_deref(),
        mix_ui::activity_reporter(),
    )
    .await?;

    if let Some(note) = removed.restored.and_then(crate::explain::change::restored) {
        mix_ui::info(note);
    }

    if json {
        println!("{}", removed.to_json());
        return Ok(ExitCode::SUCCESS);
    }

    if !removed.skipped.is_empty() {
        mix_ui::skipped(format!("not installed: {}", removed.skipped.join(", ")));
    }
    if removed.changed_nothing() {
        mix_ui::ok("nothing to remove");
    } else {
        mix_ui::ok(format!("removed: {}", removed.removed.join(", ")));
    }
    Ok(ExitCode::SUCCESS)
}
