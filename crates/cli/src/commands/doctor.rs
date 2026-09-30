use std::process::ExitCode;

use mix_shell::ops::doctor::HealthReport;

pub async fn run(view: &crate::render::sinks::View) -> anyhow::Result<ExitCode> {
    let ctx = mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(super::enrolled_user())
        .with_render(view.sinks(mix_ui::display(true))?)
        .with_policy(super::policy());
    let reports = mix_shell::ops::doctor::audit(&ctx).await;

    if reports.iter().all(HealthReport::healthy) {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}
