use std::process::ExitCode;

use mix_app::doctor::HealthReport;
use mix_core::Category;

pub async fn run(verbose: u8) -> anyhow::Result<ExitCode> {
    let reports = mix_app::doctor::audit().await;
    render(&reports, verbose > 0);

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

fn render(reports: &[HealthReport], verbose: bool) {
    for category in Category::ALL {
        let members: Vec<&HealthReport> = reports
            .iter()
            .filter(|report| report.category == category)
            .collect();
        if members.is_empty() {
            continue;
        }

        let all_healthy = members.iter().all(|report| report.healthy);
        if all_healthy && !verbose {
            mix_ui::ok(format!("{} ({} checks)", category.label(), members.len()));
            continue;
        }

        mix_ui::header(category.label());
        for report in members {
            if report.healthy {
                mix_ui::ok(&report.name);
                continue;
            }

            let detail = report.detail.as_deref().unwrap_or("unhealthy");
            mix_ui::fail(format!("{}: {detail}", report.name));
        }
    }
}

pub fn check_failed_message(e: impl std::fmt::Display) -> String {
    format!("System health check failed: {e}\n\nRun `mix repair` to reconcile configuration drift.")
}
