use std::process::ExitCode;

use mix_shell::ops::remove::Error;

pub async fn run(
    packages: &[String],
    view: &crate::render::sinks::View,
) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile().map_err(Error::from)?;
    let ctx = mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(Some(user_config))
        .with_render(view.sinks(mix_ui::display(false))?)
        .with_policy(super::policy())
        .with_host(super::host_config());
    let _watch = crate::controls::watch(
        &ctx.scope,
        view.notices(crate::controls::CHANGE),
        std::future::pending(),
        crate::controls::Side::Client,
    );

    mix_shell::ops::remove::remove(&ctx, packages).await?;

    Ok(ExitCode::SUCCESS)
}
