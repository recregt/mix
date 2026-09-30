use std::process::ExitCode;

pub async fn run(
    packages: &[String],
    view: &crate::render::sinks::View,
) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile()?;
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

    mix_shell::ops::install::install(&ctx, packages).await?;

    Ok(ExitCode::SUCCESS)
}
