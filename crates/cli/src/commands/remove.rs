use std::process::ExitCode;

use mix_shell::ops::remove::Error;

pub async fn run(packages: &[String], json: bool) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile().map_err(Error::from)?;
    let reporters = mix_ui::reporters();
    let ctx = mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(Some(user_config))
        .with_render(crate::render::human::Human::new(
            crate::render::human::Reporters {
                downloads: reporters.downloads,
                steps: reporters.passing_steps,
                activity: reporters.activity,
            },
        ))
        .with_policy(super::policy())
        .with_host(super::host_config());
    let _watch = crate::controls::watch(
        &ctx.scope,
        crate::controls::CHANGE,
        std::future::pending(),
        crate::controls::Side::Client,
    );

    let removed = mix_shell::ops::remove::remove(&ctx, packages).await?;

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
