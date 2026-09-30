use mix_core::Category;
use mix_core::health::wire;
use mix_core::paths::PROFILE_SNIPPET_DEST;
use mix_events::v1::diagnostic::Detail;
use mix_events::v1::node_finished::Result;
use mix_events::v1::{InspectionReport, NodeFinished, RepairReport, Status};
use mix_shell::ops::doctor::HealthReport;
use mix_shell::profile::state::Source;

use crate::explain::change;
use crate::explain::target::unfixable;

pub(super) fn finished(node: &NodeFinished, printed: bool, verbose: bool) {
    let Some(result) = &node.result else {
        return;
    };
    match result {
        Result::Bootstrap(_) => {
            mix_ui::ok("mix is ready!");
            mix_ui::info("");
            mix_ui::info(format!(
                "to use installed packages in this terminal session, run:\n  source {PROFILE_SNIPPET_DEST}"
            ));
        }
        Result::Repair(repair) => repaired(&repair.reports, node.status() == Status::Cancelled),
        Result::Doctor(doctor) => audited(&doctor.reports, verbose),
        Result::Install(install) => {
            reset(install.restored);
            if printed {
                changed(
                    &install.added,
                    &install.skipped,
                    "already installed",
                    "nothing to install",
                    "installed",
                );
            }
        }
        Result::Remove(remove) => {
            reset(remove.restored);
            if printed {
                changed(
                    &remove.removed,
                    &remove.skipped,
                    "not installed",
                    "nothing to remove",
                    "removed",
                );
            }
        }
        _ => {}
    }
}

fn reset(was_reset: bool) {
    if was_reset && let Some(note) = change::restored(Source::Fresh) {
        mix_ui::warn(note.message());
    }
}

fn changed(changed: &[String], skipped: &[String], skipped_as: &str, none: &str, done: &str) {
    if !skipped.is_empty() {
        mix_ui::skipped(format!("{skipped_as}: {}", skipped.join(", ")));
    }
    if changed.is_empty() {
        mix_ui::ok(none);
    } else {
        mix_ui::ok(format!("{done}: {}", changed.join(", ")));
    }
}

fn repaired(reports: &[RepairReport], interrupted: bool) {
    if reports.is_empty() && !interrupted {
        mix_ui::ok("Nothing to repair, system health is intact.");
        return;
    }
    for report in reports {
        match &report.failure {
            None if report.fixed => mix_ui::ok(format!("repaired: {}", report.target)),
            None => mix_ui::fail_about(&report.target, ""),
            Some(failure) => mix_ui::fail_about(&report.target, &why(failure)),
        }
    }
    if interrupted {
        mix_ui::fail(
            "The repair was stopped before it finished. Run `mix repair` again to finish it.",
        );
    } else if reports.iter().all(|report| report.fixed) {
        mix_ui::ok("System state repaired.");
    } else {
        mix_ui::fail("Some issues could not be repaired automatically.");
    }
}

fn why(failure: &mix_events::v1::Diagnostic) -> String {
    match &failure.detail {
        Some(Detail::Unrepairable(detail)) => {
            match crate::explain::render::unfixable_of(detail.reason) {
                Some(reason) => format!("{reason}\n{}", unfixable(reason)),
                None => failure.message.clone(),
            }
        }
        _ => failure.message.clone(),
    }
}

fn audited(inspected: &[InspectionReport], verbose: bool) {
    let reports: Vec<HealthReport> = inspected
        .iter()
        .filter_map(|report| {
            Some(HealthReport {
                name: report.target.clone(),
                category: wire::category_from(report.category())?,
                finding: match &report.finding {
                    Some(finding) => Some(wire::finding_from(finding)?),
                    None => None,
                },
            })
        })
        .collect();
    for category in Category::ALL {
        let members: Vec<&HealthReport> = reports
            .iter()
            .filter(|report| report.category == category)
            .collect();
        if members.is_empty() {
            continue;
        }
        if members.iter().all(|report| report.healthy()) && !verbose {
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
    if reports.iter().all(HealthReport::healthy) {
        mix_ui::ok("System health is intact.");
    } else {
        mix_ui::fail(crate::explain::doctor::unhealthy(&reports).message());
    }
}
