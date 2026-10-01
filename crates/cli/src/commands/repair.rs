use std::process::ExitCode;

use mix_events::v1::RepairRequest;
use mix_events::v1::command::Request;
use mix_shell::ops::repair::Repair;

pub async fn run(view: &crate::render::sinks::View) -> anyhow::Result<ExitCode> {
    if !mix_shell::effect::accounts::is_root() {
        crate::remote::client::run(Request::Repair(RepairRequest {}), view).await?;
        return Ok(ExitCode::SUCCESS);
    }
    let _lock = super::acquire_lock()?;
    let ctx = mix_shell::Context::new(mix_exec::Scope::root())
        .with_user(super::enrolled_user())
        .with_render(view.sinks(mix_ui::display())?)
        .with_policy(super::policy())
        .with_host(super::host_config());
    let _watch = crate::controls::watch(&ctx.scope, view.notices(crate::controls::repair()));
    let Repair {
        reports,
        interrupted,
    } = mix_shell::ops::repair::repair(&ctx).await;

    if interrupted || !reports.iter().all(|report| report.fixed) {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}
