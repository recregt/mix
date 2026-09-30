#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use std::time::Duration;

use mix_core::health::wire;
use mix_core::paths::PROFILE_SNIPPET_DEST;
use mix_events::Detail;
use mix_events::v1::diagnostic::Detail as Found;
use mix_events::v1::node_finished::Result;
use mix_events::v1::{InspectionReport, NodeFinished, RepairReport, Status as Ended};
use mix_shell::ops::doctor::HealthReport;
use mix_shell::profile::state::Source;
use mix_ui::{Report, Severity, Status};

use crate::explain::change;
use crate::explain::target::unfixable;

pub(crate) fn took(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{:.2}s", elapsed.as_secs_f64())
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn words(severity: Severity, words: &crate::explain::Diagnostic) {
    let (summary, hint) = words.parts();
    mix_ui::report(
        severity,
        &Report {
            summary,
            helps: hint.into_iter().collect(),
            ..Report::default()
        },
    );
}

pub(super) fn finished(node: &NodeFinished, printed: bool, level: Detail, elapsed: Duration) {
    let Some(result) = &node.result else {
        return;
    };
    match result {
        Result::Bootstrap(_) => {
            mix_ui::status(Status::Finished, &format!("setup in {}", took(elapsed)));
            mix_ui::note(&format!(
                "to use installed packages in this terminal session, run `source {PROFILE_SNIPPET_DEST}`"
            ));
        }
        Result::Repair(repair) => {
            repaired(&repair.reports, node.status() == Ended::Cancelled, elapsed)
        }
        Result::Doctor(doctor) => audited(&doctor.reports, level),
        Result::Install(install) => {
            reset(install.restored, level);
            if printed {
                changed(
                    &install.added,
                    &install.skipped,
                    "already installed",
                    Status::Installed,
                    elapsed,
                );
            }
        }
        Result::Remove(remove) => {
            reset(remove.restored, level);
            if printed {
                changed(
                    &remove.removed,
                    &remove.skipped,
                    "not installed",
                    Status::Removed,
                    elapsed,
                );
            }
        }
        Result::Inspection(_) | Result::Process(_) => {}
    }
}

fn reset(was_reset: bool, level: Detail) {
    if was_reset
        && level >= Detail::Step
        && let Some(note) = change::restored(Source::Fresh)
    {
        words(Severity::Warning, &note);
    }
}

fn changed(changed: &[String], skipped: &[String], why: &str, done: Status, elapsed: Duration) {
    if !skipped.is_empty() {
        mix_ui::status(Status::Ignored, &format!("{} ({why})", skipped.join(", ")));
    }
    if !changed.is_empty() {
        mix_ui::status(
            done,
            &format!("{} in {}", changed.join(", "), took(elapsed)),
        );
    }
}

fn repaired(reports: &[RepairReport], interrupted: bool, elapsed: Duration) {
    let fixed: Vec<&str> = reports
        .iter()
        .filter(|report| report.fixed)
        .map(|report| report.target.as_str())
        .collect();
    for report in reports {
        match &report.failure {
            None if report.fixed => {}
            None => mix_ui::report(
                Severity::Warning,
                &Report {
                    summary: &format!("couldn't repair {}", report.target),
                    ..Report::default()
                },
            ),
            Some(failure) => {
                let why = why(failure);
                mix_ui::report(
                    Severity::Warning,
                    &Report {
                        summary: &format!("couldn't repair {}", report.target),
                        helps: vec![&why],
                        ..Report::default()
                    },
                );
            }
        }
    }
    if interrupted {
        words(
            Severity::Error,
            &crate::explain::Diagnostic::hinting(
                "the repair was stopped before it finished",
                "run `mix repair` again to finish it",
            ),
        );
        return;
    }
    if !fixed.is_empty() {
        mix_ui::status(
            Status::Repaired,
            &format!("{} in {}", fixed.join(", "), took(elapsed)),
        );
    }
    if reports.is_empty() {
        mix_ui::status(Status::Checked, "system, nothing to repair");
    } else if fixed.len() < reports.len() {
        words(
            Severity::Error,
            &crate::explain::Diagnostic::new("some problems couldn't be repaired automatically"),
        );
    }
}

fn why(failure: &mix_events::v1::Diagnostic) -> String {
    match &failure.detail {
        Some(Found::Unrepairable(detail)) => {
            match crate::explain::render::unfixable_of(detail.reason) {
                Some(reason) => format!("{reason}\n{}", unfixable(reason)),
                None => failure.message.clone(),
            }
        }
        Some(
            Found::Io(_)
            | Found::Command(_)
            | Found::Network(_)
            | Found::Integrity(_)
            | Found::Target(_)
            | Found::Host(_)
            | Found::Path(_)
            | Found::Packages(_)
            | Found::Format(_)
            | Found::Lock(_)
            | Found::Steps(_)
            | Found::Conflict(_)
            | Found::Unit(_),
        )
        | None => failure.message.clone(),
    }
}

fn audited(inspected: &[InspectionReport], level: Detail) {
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
    for report in &reports {
        if report.healthy() {
            if level >= Detail::Action {
                mix_ui::status(Status::Checked, &report.name);
            }
            continue;
        }
        let check = crate::explain::doctor::check(report);
        let mut lines = check.splitn(2, '\n');
        let found = lines.next().unwrap_or_default();
        mix_ui::report(
            Severity::Warning,
            &Report {
                summary: &format!("{}: {found}", report.name),
                helps: lines.collect(),
                ..Report::default()
            },
        );
    }
    if reports.iter().all(HealthReport::healthy) {
        mix_ui::status(
            Status::Checked,
            &format!("{} targets, no problems", reports.len()),
        );
    } else {
        words(
            Severity::Error,
            &crate::explain::doctor::unhealthy(&reports),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_run_reads_in_seconds_and_a_long_one_in_minutes() {
        assert_eq!(took(Duration::from_millis(14_230)), "14.23s");
        assert_eq!(took(Duration::from_secs(72)), "1m 12s");
    }
}
