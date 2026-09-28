use std::process::ExitCode;

use mix_app::remove::Error;

pub async fn run(
    packages: &[String],
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    json: bool,
) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile().map_err(Error::from)?;
    let reporters = mix_ui::reporters();
    let ctx = mix_app::Context::new(mix_exec::Scope::root())
        .with_user(Some(user_config))
        .with_reporters(mix_app::Reporters {
            downloads: reporters.downloads,
            steps: reporters.passing_steps,
            activity: reporters.activity,
        })
        .with_env(super::request_env(mirror, mirror_key))
        .with_host(super::host_config());
    let _watch = crate::interrupt::watch(
        &ctx.scope,
        crate::interrupt::CHANGE,
        std::future::pending(),
        crate::interrupt::Side::Client,
    );

    let removed = mix_app::remove::remove(&ctx, packages).await?;

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
