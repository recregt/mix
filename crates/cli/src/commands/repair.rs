use std::process::ExitCode;

use mix_app::repair::RepairReport;

pub async fn run(verbosity: u8) -> anyhow::Result<ExitCode> {
    let reports = if mix_core::privilege::is_root() {
        let _lock = super::acquire_lock()?;
        let user_config = mix_app::resolve_existing_user_config();
        mix_app::repair::repair(user_config.as_ref()).await
    } else {
        crate::remote::client::repair(verbosity).await?
    };

    if reports.is_empty() {
        mix_ui::ok("Nothing to repair, system health is intact.");
        return Ok(ExitCode::SUCCESS);
    }

    render(&reports);

    if reports.iter().all(|report| report.fixed) {
        mix_ui::ok("System state repaired.");
        Ok(ExitCode::SUCCESS)
    } else {
        mix_ui::fail("Some issues could not be repaired automatically.");
        Ok(ExitCode::FAILURE)
    }
}

fn render(reports: &[RepairReport]) {
    for report in reports {
        match &report.error {
            None => mix_ui::ok(format!("repaired: {}", report.name)),
            Some(error) => mix_ui::fail_about(&report.name, &crate::explain::target::report(error)),
        }
    }
}
