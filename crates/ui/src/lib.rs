#![allow(clippy::disallowed_methods)]

use std::io::{IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod activity;
mod display;
mod problem;
mod progress;
mod status;
pub mod text;

pub use display::{Display, Silent, StepLine, display};
pub use problem::{Labels, Lines, Problem, problem_text, problem_to};
pub use progress::{init, live_style};
pub use status::Status;
pub use text::{Help, Note, Phrase};

use status::{GUTTER, Tone};

static PROGRESS: AtomicBool = AtomicBool::new(true);

const GREEN: &str = "\u{1b}[92m";
const RED: &str = "\u{1b}[91m";
const YELLOW: &str = "\u{1b}[33m";
const CYAN: &str = "\u{1b}[96m";
const BOLD: &str = "\u{1b}[1m";
const RESET: &str = "\u{1b}[0m";

const MAX_CAUSES: usize = 4;

pub(crate) fn set_progress_enabled(enabled: bool) {
    PROGRESS.store(enabled, Ordering::Relaxed);
}

pub fn progress_enabled() -> bool {
    PROGRESS.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

static CHOICE: OnceLock<ColorChoice> = OnceLock::new();

pub fn set_color(choice: ColorChoice) {
    let _ = CHOICE.set(choice);
    console::set_colors_enabled_stderr(stderr_colors());
    console::set_colors_enabled(stdout_colors());
}

#[derive(Debug, Clone, Copy, Default)]
struct Environment {
    no_color: bool,
    clicolor_force: bool,
    clicolor: Option<bool>,
    term_supports_color: bool,
    ci: bool,
}

impl Environment {
    fn read() -> Self {
        Self {
            no_color: anstyle_query::no_color(),
            clicolor_force: anstyle_query::clicolor_force(),
            clicolor: anstyle_query::clicolor(),
            term_supports_color: anstyle_query::term_supports_color(),
            ci: anstyle_query::is_ci(),
        }
    }
}

fn decide(choice: ColorChoice, is_terminal: bool, env: Environment) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto if env.no_color => false,
        ColorChoice::Auto if env.clicolor_force => true,
        ColorChoice::Auto if env.clicolor == Some(false) => false,
        ColorChoice::Auto => {
            is_terminal && (env.term_supports_color || env.clicolor == Some(true) || env.ci)
        }
    }
}

fn colors_enabled(is_terminal: bool) -> bool {
    let choice = CHOICE.get().copied().unwrap_or(ColorChoice::Auto);
    decide(choice, is_terminal, Environment::read())
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

pub struct Spaced<O> {
    inner: O,
    gap: AtomicBool,
}

impl<O: Out> Spaced<O> {
    pub fn new(inner: O) -> Self {
        Self {
            inner,
            gap: AtomicBool::new(false),
        }
    }
}

impl<O: Out> Out for Spaced<O> {
    fn line(&self, text: &str) {
        if self.gap.swap(false, Ordering::Relaxed) {
            self.inner.line("");
        }
        self.inner.line(text);
        if text.contains('\n') {
            self.gap.store(true, Ordering::Relaxed);
        }
    }

    fn colours(&self) -> bool {
        self.inner.colours()
    }
}

impl<O: Out + ?Sized> Out for std::sync::Arc<O> {
    fn line(&self, text: &str) {
        (**self).line(text);
    }

    fn colours(&self) -> bool {
        (**self).colours()
    }
}

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

#[derive(Debug)]
pub struct Report<'a> {
    code: Option<&'a str>,
    summary: &'a str,
    note: Option<&'a str>,
    help: Option<&'a str>,
    causes: Vec<String>,
}

impl<'a> Report<'a> {
    pub fn new(summary: &'a Phrase) -> Self {
        Self::checked(summary.as_str(), None, None)
    }

    #[doc(hidden)]
    pub fn checked(summary: &'a str, note: Option<&'a str>, help: Option<&'a str>) -> Self {
        Self {
            code: None,
            summary,
            note,
            help,
            causes: Vec::new(),
        }
    }

    pub fn code(mut self, code: Option<&'a str>) -> Self {
        self.code = code;
        self
    }

    pub fn note(mut self, note: &'a Note) -> Self {
        self.note = Some(note.as_str());
        self
    }

    pub fn help(mut self, help: &'a Help) -> Self {
        self.help = Some(help.as_str());
        self
    }

    pub fn causes(mut self, causes: Vec<String>) -> Self {
        self.causes = causes;
        self
    }

    pub fn has_help(&self) -> bool {
        self.help.is_some()
    }
}

fn write_label(out: &mut String, colour: &str, label: &str, text: &str, colours: bool) {
    let mut lines = text.lines();
    painted(out, colour, label, colours);
    out.push_str(": ");
    out.push_str(lines.next().unwrap_or_default());
    for line in lines {
        out.push('\n');
        for _ in 0..label.len() + 2 {
            out.push(' ');
        }
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
    let subs = report
        .note
        .map(|note| (GREEN, "note", note))
        .into_iter()
        .chain(report.help.map(|help| (CYAN, "help", help)));
    for (index, (colour, label, text)) in subs.enumerate() {
        out.push_str(if index == 0 { "\n\n" } else { "\n" });
        write_label(&mut out, colour, label, text, colours);
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

pub fn report_to(out: &dyn Out, severity: Severity, report: &Report<'_>) {
    out.line(&report_text(severity, report, out.colours()));
}

pub fn report(severity: Severity, report: &Report<'_>) {
    report_to(&Stderr, severity, report);
}

pub fn note_to(out: &dyn Out, note: &Note, help: Option<&Help>) {
    let colours = out.colours();
    let mut line = String::new();
    write_label(&mut line, GREEN, "note", note.as_str(), colours);
    if let Some(help) = help {
        line.push('\n');
        write_label(&mut line, CYAN, "help", help.as_str(), colours);
    }
    out.line(&line);
}

pub fn note(note: &Note, help: Option<&Help>) {
    note_to(&Stderr, note, help);
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

    #[test]
    fn color_follows_the_flag_then_the_environment_as_cargo_does() {
        let terminal = Environment {
            term_supports_color: true,
            ..Environment::default()
        };
        assert!(decide(ColorChoice::Auto, true, terminal));
        assert!(!decide(ColorChoice::Auto, false, terminal));
        assert!(!decide(
            ColorChoice::Auto,
            true,
            Environment {
                no_color: true,
                ..terminal
            }
        ));
        assert!(decide(
            ColorChoice::Auto,
            false,
            Environment {
                clicolor_force: true,
                ..Environment::default()
            }
        ));
        assert!(!decide(
            ColorChoice::Auto,
            true,
            Environment {
                clicolor: Some(false),
                ..terminal
            }
        ));
        assert!(!decide(ColorChoice::Auto, true, Environment::default()));
        assert!(decide(
            ColorChoice::Auto,
            true,
            Environment {
                ci: true,
                ..Environment::default()
            }
        ));
        assert!(decide(ColorChoice::Always, false, Environment::default()));
        assert!(!decide(ColorChoice::Never, true, terminal));
    }

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
    fn a_warning_names_its_code_then_its_note_and_help_follow_a_blank_line() {
        let summary = phrase!("the change was made but not recorded in git");
        let note = note!("`git` failed while recording it");
        let help = help!("run `mix repair` to record it");
        let text = report_text(
            Severity::Warning,
            &Report::new(&summary)
                .code(Some("git-record-failed"))
                .note(&note)
                .help(&help),
            false,
        );
        assert_eq!(
            text,
            "warning[git-record-failed]: the change was made but not recorded in git\n\nnote: `git` failed while recording it\nhelp: run `mix repair` to record it"
        );
    }

    #[test]
    fn an_error_gives_its_help_then_each_cause_in_its_own_block_as_cargo_does() {
        let summary = phrase!("couldn't install ripgrep");
        let help = help!("run it again with `-v`\nto see each step");
        let text = report_text(
            Severity::Error,
            &Report::new(&summary).help(&help).causes(vec![
                "`nix build` exited with status 1".into(),
                "error: \u{1b}[31mbuilder failed\u{1b}[0m\n\n  at home.nix:10".into(),
            ]),
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
        note_to(
            &seen,
            &note!("cancelling"),
            Some(&help!("press Ctrl-C again to stop now")),
        );
        assert_eq!(
            seen.0.lock().unwrap().as_slice(),
            ["note: cancelling\nhelp: press Ctrl-C again to stop now"]
        );
    }

    #[test]
    fn a_block_of_several_parts_is_set_apart_from_what_follows() {
        let spaced = Spaced::new(Recorder::default());
        spaced.line("  Installing hello");
        spaced.line("warning: it was not recorded\n\nhelp: run `mix repair` to record it");
        spaced.line("   Installed hello in 1.00s");
        spaced.line("     Checked system in 1.00s");
        assert_eq!(
            spaced.inner.0.lock().unwrap().as_slice(),
            [
                "  Installing hello",
                "warning: it was not recorded\n\nhelp: run `mix repair` to record it",
                "",
                "   Installed hello in 1.00s",
                "     Checked system in 1.00s",
            ]
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
