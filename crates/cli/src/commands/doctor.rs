use std::process::ExitCode;

use tracing::Instrument;

use mix_shell::ops::doctor::HealthReport;

pub async fn run(view: &crate::render::sinks::View) -> anyhow::Result<ExitCode> {
    let reporters = mix_ui::reporters();
    let ctx = mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(super::enrolled_user())
        .with_render(view.sinks(crate::render::human::Reporters {
            downloads: reporters.downloads,
            steps: reporters.steps,
            activity: reporters.activity,
        })?)
        .with_policy(super::policy());
    let reports = mix_shell::ops::doctor::audit(&ctx)
        .instrument(ctx.span())
        .await;

    if reports.iter().all(HealthReport::healthy) {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}
