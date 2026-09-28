use std::process::ExitCode;

use mix_app::remove::Error;

pub async fn run(
    packages: &[String],
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    json: bool,
) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile().map_err(Error::from)?;
    let scope = mix_exec::Scope::root();
    let _watch = crate::interrupt::watch(&scope, crate::interrupt::CHANGE, std::future::pending());

    let removed = mix_app::remove::remove(
        &user_config,
        packages,
        mirror,
        mirror_key,
        mix_ui::activity_reporter(),
        &scope,
    )
    .await?;

    if let Some(note) = removed.restored.and_then(crate::explain::change::restored) {
        mix_ui::warn(note.message());
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
