use std::process::ExitCode;

use mix_shell::ops::doctor::HealthReport;

pub async fn run(verbose: u8) -> anyhow::Result<ExitCode> {
    let reporters = mix_ui::reporters();
    let ctx = mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(super::enrolled_user())
        .with_render(
            crate::render::human::Human::new(crate::render::human::Reporters {
                downloads: reporters.downloads,
                steps: reporters.steps,
                activity: reporters.activity,
            })
            .verbose(verbose > 0),
        )
        .with_policy(super::policy());
    let reports = mix_shell::ops::doctor::audit(&ctx).await;

    if reports.iter().all(HealthReport::healthy) {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}
