use std::process::ExitCode;

use mix_shell::ops::remove::Error;

pub async fn run(
    packages: &[String],
    json: bool,
    exit: &crate::render::human::Exit,
) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile().map_err(Error::from)?;
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
            exit,
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

    if json {
        println!("{}", removed.to_json());
    }
    Ok(ExitCode::SUCCESS)
}
