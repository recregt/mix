use std::process::ExitCode;

use mix_shell::ops::repair::Repair;

pub async fn run(verbosity: u8, exit: &crate::render::human::Exit) -> anyhow::Result<ExitCode> {
    let Repair {
        reports,
        interrupted,
    } = if mix_shell::effect::accounts::is_root() {
        let _lock = super::acquire_lock()?;
        let reporters = mix_ui::reporters();
        let ctx = mix_shell::Context::new(mix_exec::Scope::root())
            .with_user(super::enrolled_user())
            .with_render(
                crate::render::human::Human::new(crate::render::human::Reporters {
                    downloads: reporters.downloads,
                    steps: reporters.steps,
                    activity: reporters.activity,
                })
                .exit_to(exit),
            )
            .with_policy(super::policy())
            .with_host(super::host_config());
        let _watch = crate::controls::watch(
            &ctx.scope,
            crate::controls::REPAIR,
            std::future::pending(),
            crate::controls::Side::Client,
        );
        mix_shell::ops::repair::repair(&ctx).await
    } else {
        crate::remote::client::repair(verbosity, exit).await?
    };

    if interrupted || !reports.iter().all(|report| report.fixed) {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}
