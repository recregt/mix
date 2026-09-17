use std::process::ExitCode;

use mix_app::doctor::HealthReport;
use mix_core::Category;

pub async fn run(verbose: u8) -> anyhow::Result<ExitCode> {
    let user_config = mix_app::resolve_existing_user_config();
    let reports = mix_app::doctor::audit(user_config.as_ref()).await;
    render(&reports, verbose > 0);

    if reports.iter().all(HealthReport::healthy) {
        mix_ui::ok("System health is intact.");
        Ok(ExitCode::SUCCESS)
    } else {
        mix_ui::fail(crate::explain::doctor::unhealthy(&reports).message());
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

        let all_healthy = members.iter().all(|report| report.healthy());
        if all_healthy && !verbose {
            mix_ui::ok(format!("{} ({} checks)", category.label(), members.len()));
            continue;
        }

        mix_ui::header(category.label());
        for report in members {
            if report.healthy() {
                mix_ui::ok(&report.name);
                continue;
            }

            mix_ui::fail_about(&report.name, &crate::explain::doctor::check(report));
        }
    }
}
