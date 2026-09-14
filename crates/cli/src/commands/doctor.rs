use std::process::ExitCode;

use mix_app::doctor::HealthReport;

pub async fn run() -> anyhow::Result<ExitCode> {
    let reports = mix_app::doctor::audit().await;
    render(&reports);

    if reports.iter().all(|report| report.healthy) {
        mix_ui::ok("System health is intact.");
        Ok(ExitCode::SUCCESS)
    } else {
        mix_ui::fail(
            "System health check failed.\n\nRun `mix repair` to reconcile configuration drift.",
        );
        Ok(ExitCode::FAILURE)
    }
}

fn render(reports: &[HealthReport]) {
    for report in reports {
        if report.healthy {
            mix_ui::ok(&report.name);
            continue;
        }

        let detail = report.detail.as_deref().unwrap_or("unhealthy");
        mix_ui::fail(format!("{}: {detail}", report.name));
    }
}

pub fn check_failed_message(e: impl std::fmt::Display) -> String {
    format!("System health check failed: {e}\n\nRun `mix repair` to reconcile configuration drift.")
}
