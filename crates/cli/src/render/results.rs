#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use std::time::Duration;

use mix_core::health::wire;
use mix_core::paths::PROFILE_SNIPPET_DEST;
use mix_events::Detail;
use mix_events::v1::node_finished::Result;
use mix_events::v1::{InspectionReport, NodeFinished, RepairReport, Status as Ended};
use mix_shell::ops::doctor::HealthReport;
use mix_shell::profile::state::Source;
use mix_ui::{Out, Report, Severity, Status};

use crate::explain::change;

pub(crate) fn took(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{:.2}s", elapsed.as_secs_f64())
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn words(out: &dyn Out, severity: Severity, words: &crate::explain::Diagnostic) {
    mix_ui::report_to(out, severity, &words.report());
}

pub(super) fn finished(
    out: &dyn Out,
    node: &NodeFinished,
    printed: bool,
    level: Detail,
    elapsed: Duration,
) {
    let Some(result) = &node.result else {
        return;
    };
    let chatty = level >= Detail::Step;
    match result {
        Result::Bootstrap(_) => {
            if chatty {
                mix_ui::status_to(
                    out,
                    Status::Finished,
                    &format!("setup in {}", took(elapsed)),
                );
                mix_ui::note_to(
                    out,
                    &mix_ui::note!("new terminal sessions see the installed packages"),
                    Some(&mix_ui::help!(
                        "run `source {PROFILE_SNIPPET_DEST}` to use them in this one"
                    )),
                );
            }
        }
        Result::Repair(repair) => repaired(
            out,
            &repair.reports,
            node.status() == Ended::Cancelled,
            chatty,
            elapsed,
        ),
        Result::Doctor(doctor) => audited(out, &doctor.reports, level, elapsed),
        Result::Install(install) => {
            reset(out, install.restored, level);
            if printed && chatty {
                changed(
                    out,
                    &install.added,
                    &install.skipped,
                    "already installed",
                    Status::Installed,
                    elapsed,
                );
            }
        }
        Result::Remove(remove) => {
            reset(out, remove.restored, level);
            if printed && chatty {
                changed(
                    out,
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

fn reset(out: &dyn Out, was_reset: bool, level: Detail) {
    if was_reset
        && level >= Detail::Step
        && let Some(note) = change::restored(Source::Fresh)
    {
        words(out, Severity::Warning, &note);
    }
}

fn changed(
    out: &dyn Out,
    changed: &[String],
    skipped: &[String],
    why: &str,
    done: Status,
    elapsed: Duration,
) {
    if !skipped.is_empty() {
        mix_ui::status_to(
            out,
            Status::Ignored,
            &format!("{} ({why})", skipped.join(", ")),
        );
    }
    if !changed.is_empty() {
        mix_ui::status_to(
            out,
            done,
            &format!("{} in {}", changed.join(", "), took(elapsed)),
        );
    }
}

fn repaired(
    out: &dyn Out,
    reports: &[RepairReport],
    interrupted: bool,
    chatty: bool,
    elapsed: Duration,
) {
    let fixed: Vec<&str> = reports
        .iter()
        .filter(|report| report.fixed)
        .map(|report| report.target.as_str())
        .collect();
    if chatty {
        for report in reports {
            match &report.failure {
                None if report.fixed => {}
                None => mix_ui::report_to(
                    out,
                    Severity::Warning,
                    &Report::new(&mix_ui::phrase!("couldn't repair {}", report.target)),
                ),
                Some(failure) => {
                    let fault = mix_events::Fault::Failed(failure.clone());
                    let action = format!("repair {}", report.target);
                    let words = crate::explain::render(
                        &fault,
                        &crate::explain::Context {
                            command: crate::explain::repair::COMMAND,
                            action: &action,
                        },
                    );
                    mix_ui::report_to(
                        out,
                        Severity::Warning,
                        &Report {
                            causes: crate::explain::evidence(&fault),
                            ..words.report()
                        },
                    );
                }
            }
        }
    }
    if interrupted {
        if chatty {
            words(
                out,
                Severity::Warning,
                &crate::explain::Diagnostic::new(mix_ui::phrase!(
                    "the repair was stopped before it finished"
                ))
                .help(mix_ui::help!("run `mix repair` again to finish it")),
            );
        }
        return;
    }
    if chatty && !fixed.is_empty() {
        mix_ui::status_to(
            out,
            Status::Repaired,
            &format!("{} in {}", fixed.join(", "), took(elapsed)),
        );
    }
    if reports.is_empty() {
        if chatty {
            mix_ui::status_to(
                out,
                Status::Checked,
                &format!("system in {}", took(elapsed)),
            );
        }
    } else if fixed.len() < reports.len() {
        words(
            out,
            Severity::Error,
            &crate::explain::Diagnostic::new(mix_ui::phrase!(
                "some problems couldn't be repaired automatically"
            )),
        );
    }
}

fn audited(out: &dyn Out, inspected: &[InspectionReport], level: Detail, elapsed: Duration) {
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
    let chatty = level >= Detail::Step;
    for report in &reports {
        if report.healthy() {
            if level >= Detail::Action {
                mix_ui::status_to(out, Status::Checked, &report.name);
            }
            continue;
        }
        if !chatty {
            continue;
        }
        words(
            out,
            Severity::Warning,
            &crate::explain::doctor::check(report),
        );
    }
    if reports.iter().all(HealthReport::healthy) {
        if chatty {
            mix_ui::status_to(
                out,
                Status::Checked,
                &format!("system in {}", took(elapsed)),
            );
        }
    } else {
        words(
            out,
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
