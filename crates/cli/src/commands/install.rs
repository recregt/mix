use std::process::ExitCode;

pub async fn run(packages: &[String], json: bool) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile()?;
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

    let installed = mix_shell::ops::install::install(&ctx, packages).await?;

    if let Some(note) = installed
        .restored
        .and_then(crate::explain::change::restored)
    {
        mix_ui::warn(note.message());
    }

    // One line of JSON on stdout and nothing else, so a script never has to parse prose.
    if json {
        println!("{}", installed.to_json());
        return Ok(ExitCode::SUCCESS);
    }

    if !installed.skipped.is_empty() {
        mix_ui::skipped(format!(
            "already installed: {}",
            installed.skipped.join(", ")
        ));
    }
    if installed.changed_nothing() {
        mix_ui::ok("nothing to install");
    } else {
        mix_ui::ok(format!("installed: {}", installed.added.join(", ")));
    }
    Ok(ExitCode::SUCCESS)
}
