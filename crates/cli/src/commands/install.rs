use std::process::ExitCode;

use tracing::Instrument;

pub async fn run(
    packages: &[String],
    view: &crate::render::sinks::View,
) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile()?;
    let reporters = mix_ui::reporters();
    let ctx = mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(Some(user_config))
        .with_render(view.sinks(crate::render::human::Reporters {
            downloads: reporters.downloads,
            steps: reporters.passing_steps,
            activity: reporters.activity,
        })?)
        .with_policy(super::policy())
        .with_host(super::host_config());
    let _watch = crate::controls::watch(
        &ctx,
        crate::controls::CHANGE,
        std::future::pending(),
        crate::controls::Side::Client,
    );

    mix_shell::ops::install::install(&ctx, packages)
        .instrument(ctx.span())
        .await?;

    Ok(ExitCode::SUCCESS)
}
