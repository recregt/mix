use std::process::ExitCode;

pub async fn run(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    view: &crate::render::sinks::View,
) -> anyhow::Result<ExitCode> {
    let policy = super::requested_policy(mirror, mirror_key)?;
    if mix_shell::effect::accounts::is_root() {
        let _lock = super::acquire_lock()?;
        let ctx = mix_shell::Context::new(mix_exec::Scope::root())
            .with_user(
                mix_shell::effect::accounts::invoking_user()
                    .and_then(mix_shell::profile::user_config_for),
            )
            .with_render(view.sinks(mix_ui::display())?)
            .with_policy(policy)
            .with_host(super::host_config());
        let _watch = crate::controls::watch(&ctx.scope, view.notices(crate::controls::bootstrap()));
        mix_shell::ops::bootstrap::bootstrap(&ctx, force).await?;
    } else {
        let request = crate::remote::client::bootstrap_request(mirror, mirror_key, force);
        crate::remote::client::run(request, view).await?;
    }
    Ok(ExitCode::SUCCESS)
}
