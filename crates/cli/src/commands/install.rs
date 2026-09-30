use std::process::ExitCode;

pub async fn run(packages: &[String], json: bool) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile()?;
    let reporters = mix_ui::reporters();
    let ctx = mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(Some(user_config))
        .with_render(super::human(
            crate::render::human::Reporters {
                downloads: reporters.downloads,
                steps: reporters.passing_steps,
                activity: reporters.activity,
            },
            json,
        ))
        .with_policy(super::policy())
        .with_host(super::host_config());
    let _watch = crate::controls::watch(
        &ctx.scope,
        crate::controls::CHANGE,
        std::future::pending(),
        crate::controls::Side::Client,
    );

    let installed = mix_shell::ops::install::install(&ctx, packages).await?;

    if json {
        println!("{}", installed.to_json());
    }
    Ok(ExitCode::SUCCESS)
}
