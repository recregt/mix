#![allow(clippy::disallowed_methods)]

use std::io::{IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod activity;
mod display;
mod progress;
mod status;

pub use display::{Display, Silent, StepLine, display};
pub use progress::{init, live_style};
pub use status::Status;

use status::{GUTTER, Tone};

static PROGRESS: AtomicBool = AtomicBool::new(true);

const GREEN: &str = "\u{1b}[32m";
const RED: &str = "\u{1b}[31m";
const YELLOW: &str = "\u{1b}[33m";
const CYAN: &str = "\u{1b}[36m";
const BOLD: &str = "\u{1b}[1m";
const RESET: &str = "\u{1b}[0m";

const MAX_CAUSES: usize = 4;

pub(crate) fn set_progress_enabled(enabled: bool) {
    PROGRESS.store(enabled, Ordering::Relaxed);
}

pub fn progress_enabled() -> bool {
    PROGRESS.load(Ordering::Relaxed)
}

fn colors_enabled(is_terminal: bool) -> bool {
    is_terminal && std::env::var_os("NO_COLOR").is_none()
}

pub(crate) fn stderr_colors() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| colors_enabled(std::io::stderr().is_terminal()))
}

fn print_line(line: &str) {
    let write = || {
        let mut out = std::io::stderr().lock();
        let _ = out
            .write_all(line.as_bytes())
            .and_then(|()| out.write_all(b"\n"));
    };
    if progress_enabled() {
        progress::board().suspend(write);
    } else {
        write();
    }
}

fn painted(out: &mut String, colour: &str, text: &str, colours: bool) {
    if colours {
        out.push_str(BOLD);
        out.push_str(colour);
        out.push_str(text);
        out.push_str(RESET);
    } else {
        out.push_str(text);
    }
}

pub fn data(text: &str) {
    let write = || {
        let mut out = std::io::stdout().lock();
        let _ = out
            .write_all(text.as_bytes())
            .and_then(|()| out.write_all(b"\n"))
            .and_then(|()| out.flush());
    };
    if progress_enabled() {
        progress::board().suspend(write);
    } else {
        write();
    }
}

pub fn status_line(status: Status, subject: &str, colours: bool) -> String {
    let text = status.text();
    let mut out = String::with_capacity(GUTTER + 1 + subject.len() + 16);
    for _ in text.len()..GUTTER {
        out.push(' ');
    }
    let colour = match status.tone() {
        Tone::Plain => GREEN,
        Tone::Caution => YELLOW,
    };
    painted(&mut out, colour, text, colours);
    out.push(' ');
    out.push_str(subject);
    out
}

pub fn status(status: Status, subject: &str) {
    print_line(&status_line(status, subject, stderr_colors()));
}

pub fn output_line(line: &str) -> String {
    let printable = activity::printable(line);
    let mut out = String::with_capacity(GUTTER + 3 + printable.len());
    for _ in 0..=GUTTER {
        out.push(' ');
    }
    out.push_str("| ");
    out.push_str(&printable);
    out
}

pub fn output(line: &str) {
    print_line(&output_line(line));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Default)]
pub struct Report<'a> {
    pub code: Option<&'a str>,
    pub summary: &'a str,
    pub causes: Vec<String>,
    pub helps: Vec<&'a str>,
}

fn write_help(out: &mut String, text: &str, colours: bool) {
    let mut lines = text.lines();
    out.push_str("\n  ");
    painted(out, CYAN, "help", colours);
    out.push_str(": ");
    out.push_str(lines.next().unwrap_or_default());
    for line in lines {
        out.push_str("\n        ");
        out.push_str(line);
    }
}

pub fn report_text(severity: Severity, report: &Report<'_>, colours: bool) -> String {
    let (label, colour) = match severity {
        Severity::Error => ("error", RED),
        Severity::Warning => ("warning", YELLOW),
    };
    let mut out = String::with_capacity(report.summary.len() + 64);
    match report.code {
        Some(code) => painted(&mut out, colour, &format!("{label}[{code}]"), colours),
        None => painted(&mut out, colour, label, colours),
    }
    out.push_str(": ");
    out.push_str(report.summary);
    if !report.causes.is_empty() {
        out.push_str("\n\nCaused by:");
        for cause in &report.causes {
            for line in cause.lines() {
                out.push_str("\n  ");
                out.push_str(&activity::printable(line));
            }
        }
    }
    for help in &report.helps {
        write_help(&mut out, help, colours);
    }
    out
}

pub fn house_style(text: &str) -> bool {
    let starts_lowercase = text
        .chars()
        .next()
        .is_none_or(|first| !first.is_uppercase());
    let unfinished = !text.trim_end().ends_with('.');
    starts_lowercase && unfinished
}

pub fn report(severity: Severity, report: &Report<'_>) {
    debug_assert!(house_style(report.summary), "{:?}", report.summary);
    debug_assert!(
        report.helps.iter().all(|help| house_style(help)),
        "{:?}",
        report.helps
    );
    print_line(&report_text(severity, report, stderr_colors()));
}

pub fn note(text: &str) {
    debug_assert!(house_style(text), "{text:?}");
    let mut out = String::new();
    painted(&mut out, CYAN, "note", stderr_colors());
    out.push_str(": ");
    out.push_str(text);
    print_line(&out);
}

pub fn causes_of(first: Option<&dyn std::error::Error>, already: &str) -> Vec<String> {
    let mut causes: Vec<String> = Vec::new();
    let mut source = first;
    while let Some(cause) = source.filter(|_| causes.len() < MAX_CAUSES) {
        let text = cause.to_string();
        let text = text.trim();
        if !text.is_empty()
            && !already.contains(text)
            && !causes.iter().any(|known| known.contains(text))
        {
            causes.push(text.to_string());
        }
        source = cause.source();
    }
    causes
}

pub fn restore_terminal() {
    let mut stderr = std::io::stderr();
    if stderr.is_terminal() {
        let _ = stderr.write_all(b"\x1b[?25h");
        let _ = stderr.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Chained {
        message: &'static str,
        cause: Option<Box<Chained>>,
    }

    impl std::fmt::Display for Chained {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.message)
        }
    }

    impl std::error::Error for Chained {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.cause
                .as_deref()
                .map(|cause| cause as &(dyn std::error::Error + 'static))
        }
    }

    fn chain(messages: &[&'static str]) -> Chained {
        let mut chained: Option<Box<Chained>> = None;
        for message in messages.iter().rev() {
            chained = Some(Box::new(Chained {
                message,
                cause: chained,
            }));
        }
        *chained.expect("a chain has at least one error")
    }

    #[test]
    fn a_status_is_right_aligned_in_the_gutter() {
        assert_eq!(
            status_line(Status::Writing, "package list", false),
            "     Writing package list"
        );
        assert_eq!(
            status_line(Status::RollingBack, "package list", false),
            "Rolling back package list"
        );
    }

    #[test]
    fn a_status_colours_the_verb_and_nothing_else() {
        let line = status_line(Status::Installed, "ripgrep", true);
        assert!(line.ends_with(&format!("{RESET} ripgrep")));
        assert!(line.contains(&format!("{BOLD}{GREEN}Installed")));
    }

    #[test]
    fn a_warning_names_its_code_and_puts_the_help_under_it() {
        let text = report_text(
            Severity::Warning,
            &Report {
                code: Some("git-record-failed"),
                summary: "the change was made but not recorded in git",
                helps: vec!["`mix repair` records it"],
                ..Report::default()
            },
            false,
        );
        assert_eq!(
            text,
            "warning[git-record-failed]: the change was made but not recorded in git\n  help: `mix repair` records it"
        );
    }

    #[test]
    fn an_error_lists_its_causes_before_the_help() {
        let text = report_text(
            Severity::Error,
            &Report {
                summary: "couldn't install ripgrep",
                causes: vec![
                    "`nix build` exited with status 1".into(),
                    "error: \u{1b}[31mbuilder failed\u{1b}[0m".into(),
                ],
                helps: vec!["Run it again with `-v`\nto see each step"],
                ..Report::default()
            },
            false,
        );
        assert_eq!(
            text,
            "error: couldn't install ripgrep\n\nCaused by:\n  `nix build` exited with status 1\n  error: builder failed\n  help: Run it again with `-v`\n        to see each step"
        );
    }

    #[test]
    fn a_cause_already_said_is_not_repeated() {
        let error = chain(&["top", "connection refused", "connection refused", "dns"]);
        assert_eq!(
            causes_of(
                error.cause.as_deref().map(|c| c as &dyn std::error::Error),
                "couldn't download"
            ),
            ["connection refused", "dns"]
        );
    }

    #[test]
    fn text_after_a_label_starts_lowercase_and_has_no_full_stop() {
        assert!(house_style("the change was made but not recorded in git"));
        assert!(house_style("`mix repair` records it"));
        assert!(house_style("run it again with sudo:\n  sudo mix bootstrap"));
        assert!(!house_style("Run `mix repair` to fix them"));
        assert!(!house_style("some checks failed."));
    }

    #[test]
    fn a_chain_that_never_ends_is_cut_off() {
        let error = chain(&["a", "b", "c", "d", "e", "f", "g"]);
        assert_eq!(causes_of(Some(&error), "").len(), MAX_CAUSES);
    }

    #[test]
    fn a_programs_line_sits_under_the_gutter_without_its_escapes() {
        assert_eq!(
            output_line("\u{1b}[1mbuilding hello"),
            format!("{}| building hello", " ".repeat(GUTTER + 1))
        );
    }
}
