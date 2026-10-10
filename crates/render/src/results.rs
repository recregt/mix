#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use std::time::Duration;

use mix_events::Detail;
use mix_events::v1::command::Request;
use mix_events::v1::node_finished::Result;
use mix_events::v1::{
    CleanResult, Code, ExplainRequest, ExplainResult, InspectionReport, NodeFinished, RepairReport,
    Status as Ended,
};
use mix_ui::{Out, Report, Severity, Status};

pub(crate) fn took(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{:.2}s", elapsed.as_secs_f64())
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn words(out: &dyn Out, severity: Severity, words: &mix_explain::Diagnostic) {
    mix_ui::report_to(out, severity, &words.report());
}

pub(super) fn finished(
    out: &dyn Out,
    node: &NodeFinished,
    request: Option<&Request>,
    level: Detail,
    elapsed: Duration,
) {
    let Some(result) = &node.result else {
        return;
    };
    let chatty = level >= Detail::Step;
    match result {
        Result::Bootstrap(bootstrap) => {
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
                        "run `source {}` to use them in this one",
                        bootstrap.profile_snippet
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
            if chatty {
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
            if chatty {
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
        Result::Clean(clean) => {
            if chatty {
                mix_ui::status_to(
                    out,
                    Status::Removed,
                    &format!("{} in {}", cleaned(clean), took(elapsed)),
                );
            }
        }
        Result::Explain(explain) => mix_ui::data(&explained(explain, request)),
        Result::Inspection(_) | Result::Process(_) => {}
    }
}

fn explained(explain: &ExplainResult, request: Option<&Request>) -> String {
    let codes = explain
        .codes
        .iter()
        .filter_map(|code| Code::try_from(*code).ok());
    match request {
        Some(Request::Explain(ExplainRequest { code: Some(_) })) => codes
            .map(mix_explain::codes::explanation_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        Some(
            Request::Explain(ExplainRequest { code: None })
            | Request::Bootstrap(_)
            | Request::Install(_)
            | Request::Remove(_)
            | Request::Clean(_)
            | Request::Repair(_)
            | Request::Doctor(_),
        )
        | None => mix_explain::codes::list_text(codes),
    }
}

fn cleaned(clean: &CleanResult) -> String {
    let count = clean.generations.len();
    let generations = format!("{count} generation{}", if count == 1 { "" } else { "s" });
    match clean.freed_bytes {
        Some(freed) => format!("{generations}, {} freed", size(freed)),
        None => generations,
    }
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn reset(out: &dyn Out, was_reset: bool, level: Detail) {
    if was_reset && level >= Detail::Step {
        words(out, Severity::Warning, &mix_explain::reset());
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
                None if !report.blocked_by.is_empty() => mix_ui::report_to(
                    out,
                    Severity::Warning,
                    &Report::new(&mix_ui::phrase!(
                        "left {} alone because {} couldn't be repaired",
                        report.target,
                        report.blocked_by
                    )),
                ),
                None => mix_ui::report_to(
                    out,
                    Severity::Warning,
                    &Report::new(&mix_ui::phrase!("couldn't repair {}", report.target)),
                ),
                Some(failure) => {
                    let fault = mix_events::Fault::Failed(failure.clone());
                    let action = format!("repair {}", report.target);
                    let words = mix_explain::outcome("mix repair", &action, &fault);
                    mix_ui::report_to(
                        out,
                        Severity::Warning,
                        &words.report().causes(mix_explain::evidence(&fault)),
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
                &mix_explain::Diagnostic::new(mix_ui::phrase!(
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
            &mix_explain::Diagnostic::new(mix_ui::phrase!(
                "some problems couldn't be repaired automatically"
            )),
        );
    }
}

fn audited(out: &dyn Out, reports: &[InspectionReport], level: Detail, elapsed: Duration) {
    let chatty = level >= Detail::Step;
    for report in reports {
        if mix_explain::doctor::healthy(report) {
            if level >= Detail::Action {
                mix_ui::status_to(out, Status::Checked, &report.target);
            }
            continue;
        }
        if !chatty || !report.blocked_by.is_empty() {
            continue;
        }
        let waiting = mix_explain::doctor::waiting(report, reports);
        let words = match waiting.is_empty() {
            true => mix_explain::doctor::check(report),
            false => mix_explain::doctor::check(report).note(mix_ui::note!(
                "checked once it's fixed: {}",
                waiting.join(", ")
            )),
        };
        let labels = report.finding.as_ref().map(mix_explain::doctor::labels);
        let lines = report
            .drift
            .as_ref()
            .zip(labels.as_ref())
            .map(|(drift, labels)| mix_ui::Lines {
                path: &drift.path,
                hunks: &drift.hunks,
                labels,
            });
        mix_ui::problem_to(out, Severity::Warning, &words.problem(lines));
    }
    if reports.iter().all(mix_explain::doctor::healthy) {
        if chatty {
            mix_ui::status_to(
                out,
                Status::Checked,
                &format!("system in {}", took(elapsed)),
            );
        }
    } else {
        let verdict = mix_explain::doctor::unhealthy(reports);
        mix_ui::problem_to(out, Severity::Error, &verdict.problem(None));
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

    #[test]
    fn a_clean_counts_its_generations_and_what_the_store_freed() {
        let clean = |generations: Vec<u64>, freed_bytes| CleanResult {
            generations,
            freed_bytes,
        };

        assert_eq!(cleaned(&clean(vec![], None)), "0 generations");
        assert_eq!(cleaned(&clean(vec![4], None)), "1 generation");
        assert_eq!(
            cleaned(&clean(vec![1, 2, 3], Some(1_288_490_189))),
            "3 generations, 1.2GiB freed"
        );
        assert_eq!(size(512), "512B");
    }
}
