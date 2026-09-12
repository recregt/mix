use std::process::ExitCode;

use mix_bootstrap::HealthReport;

use super::RootStatus;
use crate::ui;

pub async fn run(fix: bool, mirror: Option<String>) -> anyhow::Result<ExitCode> {
    if fix {
        if let RootStatus::ReExecuted(code) = super::ensure_root()? {
            return Ok(code);
        }

        mix_bootstrap::reset(mirror.as_deref()).await?;
        ui::ok("System state successfully restored to pristine condition.");
        return Ok(ExitCode::SUCCESS);
    }

    if mirror.is_some() {
        ui::info("--mirror has no effect without --fix; ignoring it.");
    }

    check().await
}

pub async fn check() -> anyhow::Result<ExitCode> {
    let reports = mix_bootstrap::audit().await;
    render(&reports);

    if reports.iter().all(|report| report.healthy) {
        ui::ok("System health is intact.");
        Ok(ExitCode::SUCCESS)
    } else {
        ui::fail(
            "System health check failed.\n\nRun `mix doctor --fix` to reconcile configuration drift.",
        );
        Ok(ExitCode::FAILURE)
    }
}

fn render(reports: &[HealthReport]) {
    for report in reports {
        if report.healthy {
            ui::ok(&report.name);
            continue;
        }

        let detail = report.detail.as_deref().unwrap_or("unhealthy");
        ui::fail(format!("{}: {detail}", report.name));
        if let Some(hint) = report.hint {
            ui::info(format!("  {hint}"));
        }
    }
}

pub fn check_failed_message(e: impl std::fmt::Display) -> String {
    format!(
        "System health check failed: {e}\n\nRun `mix doctor --fix` to reconcile configuration drift."
    )
}
