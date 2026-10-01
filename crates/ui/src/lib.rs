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

fn stdout_colors() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| colors_enabled(std::io::stdout().is_terminal()))
}

pub trait Out: Send + Sync {
    fn line(&self, text: &str);
    fn colours(&self) -> bool;
}

pub struct Stderr;

impl Out for Stderr {
    fn line(&self, text: &str) {
        print_line(text);
    }

    fn colours(&self) -> bool {
        stderr_colors()
    }
}

pub struct Stdout;

impl Out for Stdout {
    fn line(&self, text: &str) {
        data(text);
    }

    fn colours(&self) -> bool {
        stdout_colors()
    }
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

pub fn status_to(out: &dyn Out, status: Status, subject: &str) {
    out.line(&status_line(status, subject, out.colours()));
}

pub fn status(status: Status, subject: &str) {
    status_to(&Stderr, status, subject);
}

pub fn output_lines(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines().map(output_line)
}

fn output_line(line: &str) -> String {
    let printable = activity::printable(line);
    let mut out = String::with_capacity(GUTTER + 3 + printable.len());
    for _ in 0..=GUTTER {
        out.push(' ');
    }
    out.push_str("| ");
    out.push_str(&printable);
    out
}

pub fn output_to(out: &dyn Out, text: &str) {
    for line in output_lines(text) {
        out.line(&line);
    }
}

pub fn output(text: &str) {
    output_to(&Stderr, text);
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
    painted(out, CYAN, "help", colours);
    out.push_str(": ");
    out.push_str(lines.next().unwrap_or_default());
    for line in lines {
        out.push_str("\n      ");
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
    for (index, help) in report.helps.iter().enumerate() {
        out.push_str(if index == 0 { "\n\n" } else { "\n" });
        write_help(&mut out, help, colours);
    }
    for cause in &report.causes {
        out.push_str("\n\nCaused by:");
        for line in cause.lines() {
            let line = activity::printable(line);
            out.push('\n');
            if !line.is_empty() {
                out.push_str("  ");
                out.push_str(&line);
            }
        }
    }
    out
}

pub fn house_style(text: &str) -> bool {
    let starts_lowercase = text
        .chars()
        .next()
        .is_none_or(|first| !first.is_uppercase());
    let unfinished = !text.trim_end().ends_with('.');
    let one_phrase = !text.contains("; ") && !text.contains(". ");
    starts_lowercase && unfinished && one_phrase
}

pub fn report_to(out: &dyn Out, severity: Severity, report: &Report<'_>) {
    out.line(&report_text(severity, report, out.colours()));
}

pub fn report(severity: Severity, report: &Report<'_>) {
    report_to(&Stderr, severity, report);
}

pub fn note_to(out: &dyn Out, text: &str, help: Option<&str>) {
    let colours = out.colours();
    let mut line = String::new();
    painted(&mut line, CYAN, "note", colours);
    line.push_str(": ");
    line.push_str(text);
    if let Some(help) = help {
        line.push('\n');
        write_help(&mut line, help, colours);
    }
    out.line(&line);
}

pub fn note(text: &str, help: Option<&str>) {
    note_to(&Stderr, text, help);
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

    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<String>>);

    impl Out for Recorder {
        fn line(&self, text: &str) {
            self.0.lock().unwrap().push(text.to_string());
        }

        fn colours(&self) -> bool {
            false
        }
    }

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
    fn a_warning_names_its_code_and_its_help_follows_a_blank_line() {
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
            "warning[git-record-failed]: the change was made but not recorded in git\n\nhelp: `mix repair` records it"
        );
    }

    #[test]
    fn an_error_gives_its_help_then_each_cause_in_its_own_block_as_cargo_does() {
        let text = report_text(
            Severity::Error,
            &Report {
                summary: "couldn't install ripgrep",
                causes: vec![
                    "`nix build` exited with status 1".into(),
                    "error: \u{1b}[31mbuilder failed\u{1b}[0m\n\n  at home.nix:10".into(),
                ],
                helps: vec!["run it again with `-v`\nto see each step"],
                ..Report::default()
            },
            false,
        );
        assert_eq!(
            text,
            "error: couldn't install ripgrep\n\nhelp: run it again with `-v`\n      to see each step\n\nCaused by:\n  `nix build` exited with status 1\n\nCaused by:\n  error: builder failed\n\n    at home.nix:10"
        );
    }

    #[test]
    fn a_note_carries_its_help_on_the_next_line() {
        let seen = Recorder::default();
        note_to(&seen, "cancelling", Some("press Ctrl-C again to stop now"));
        assert_eq!(
            seen.0.lock().unwrap().as_slice(),
            ["note: cancelling\nhelp: press Ctrl-C again to stop now"]
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
        assert!(!house_style("stopped; everything was undone"));
        assert!(!house_style("it stopped. Run it again"));
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

    #[test]
    fn a_message_of_several_lines_keeps_each_under_the_gutter() {
        let gutter = " ".repeat(GUTTER + 1);
        assert_eq!(
            output_lines("error:\r\n  at home.nix:10:7\n  Did you mean ripgrep?")
                .collect::<Vec<_>>(),
            [
                format!("{gutter}| error:"),
                format!("{gutter}|   at home.nix:10:7"),
                format!("{gutter}|   Did you mean ripgrep?"),
            ]
        );
    }
}
