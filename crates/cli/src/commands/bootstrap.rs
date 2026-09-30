use std::process::ExitCode;

pub async fn run(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    verbosity: u8,
    exit: &crate::render::human::Exit,
) -> anyhow::Result<ExitCode> {
    let policy = super::requested_policy(mirror, mirror_key)?;
    if mix_shell::effect::accounts::is_root() {
        let _lock = super::acquire_lock()?;
        let reporters = mix_ui::reporters();
        let ctx = mix_shell::Context::new(mix_exec::Scope::root())
            .with_user(
                mix_shell::effect::accounts::invoking_user()
                    .and_then(mix_shell::profile::user_config_for),
            )
            .with_render(
                crate::render::human::Human::new(crate::render::human::Reporters {
                    downloads: reporters.downloads,
                    steps: reporters.steps,
                    activity: reporters.activity,
                })
                .exit_to(exit),
            )
            .with_policy(policy)
            .with_host(super::host_config());
        let _watch = crate::controls::watch(
            &ctx.scope,
            crate::controls::BOOTSTRAP,
            std::future::pending(),
            crate::controls::Side::Client,
        );
        mix_shell::ops::bootstrap::bootstrap(&ctx, force).await?;
    } else {
        crate::remote::client::bootstrap(mirror, mirror_key, force, verbosity, exit).await?;
    }
    Ok(ExitCode::SUCCESS)
}
