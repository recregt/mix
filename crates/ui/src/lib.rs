use std::io::{IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod activity;
pub mod message;
mod progress;

pub use activity::activity_reporter;
pub use progress::{download_reporter, init_tracing, step_observer, step_style};

/// Whether anything may be drawn in place. Set once by [`init_tracing`], so a caller that never
/// initialises the output keeps the default.
static PROGRESS: AtomicBool = AtomicBool::new(true);

/// Colour of the marker a finished line keeps, written out rather than styled through a
/// formatter: a line is built in one pass, and these are constants.
const GREEN: &str = "\u{1b}[32m";
const RED: &str = "\u{1b}[31m";
const YELLOW: &str = "\u{1b}[33m";
const BOLD: &str = "\u{1b}[1m";
const RESET: &str = "\u{1b}[0m";

/// Markers a printed line opens with, and the columns they take with the space after them.
const DONE: &str = "✓";
const FAILED: &str = "✗";
const LEFT_ALONE: &str = "•";
const WARNING: &str = "!";
const MARKER_WIDTH: usize = 2;

/// How many causes are worth printing under a failure. A chain longer than this is a library
/// explaining itself to its own author, not to the person running the command.
const MAX_CAUSES: usize = 4;

pub(crate) fn set_progress_enabled(enabled: bool) {
    PROGRESS.store(enabled, Ordering::Relaxed);
}

/// Whether progress bars and live output are drawn at all.
pub fn progress_enabled() -> bool {
    PROGRESS.load(Ordering::Relaxed)
}

fn colors_enabled(is_terminal: bool) -> bool {
    is_terminal && std::env::var_os("NO_COLOR").is_none()
}

/// Neither answer can change while the process runs, and asking costs a `stat` of the stream
/// plus a walk of the environment — per printed line, on a command that prints one line per
/// check.
fn stdout_colors() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| colors_enabled(std::io::stdout().is_terminal()))
}

pub(crate) fn stderr_colors() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| colors_enabled(std::io::stderr().is_terminal()))
}

/// Prints a finished line without letting it collide with whatever is being drawn in place.
///
/// A progress bar owns the last lines of the terminal, and a plain `println!` writes straight
/// past it: the two end up interleaved on the same row, and the bar redraws over what was
/// printed. Suspending the drawing for the write is what keeps the output readable. Nothing is
/// suspended when nothing is being drawn, which costs one atomic load.
///
/// The line arrives fully built, so the write is one `write_all` under one lock rather than a
/// formatter run holding the stream for the length of the message.
fn print_line(line: &str, to_stderr: bool) {
    let write = || {
        let bytes = line.as_bytes();
        let written = if to_stderr {
            let mut out = std::io::stderr().lock();
            out.write_all(bytes).and_then(|()| out.write_all(b"\n"))
        } else {
            let mut out = std::io::stdout().lock();
            out.write_all(bytes).and_then(|()| out.write_all(b"\n"))
        };
        let _ = written;
    };

    if progress_enabled() {
        tracing_indicatif::suspend_tracing_indicatif(write);
    } else {
        write();
    }
}

/// What a printed line says about the thing it reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// It worked.
    Done,
    /// It did not.
    Failed,
    /// It was deliberately left alone, e.g. a package that is already installed.
    LeftAlone,
    /// Neither: something the user is being told.
    Plain,
    Warning,
}

impl Status {
    /// The marker the line opens with, and the colour it is drawn in.
    fn marker(self) -> Option<(&'static str, &'static str)> {
        match self {
            Status::Done => Some((DONE, GREEN)),
            Status::Failed => Some((FAILED, RED)),
            Status::LeftAlone => Some((LEFT_ALONE, YELLOW)),
            Status::Warning => Some((WARNING, YELLOW)),
            Status::Plain => None,
        }
    }
}

/// Builds a whole line — marker, message, full stop — in one buffer.
///
/// Printing costs the string the line is printed from and nothing else: the marker, the raised
/// first letter, the indent under it, the code spans and the full stop are all written in the
/// one pass, where they used to be a string per part and a formatter run on top.
pub fn status_line(status: Status, message: &str, colour: bool) -> String {
    let marker = status.marker();
    // The message, the marker, and room for the full stop and the sequences the marker and any
    // code spans are drawn with.
    let mut out = String::with_capacity(message.len() + 24);

    if let Some((glyph, colour_code)) = marker {
        if colour {
            out.push_str(colour_code);
            out.push_str(glyph);
            out.push_str(RESET);
        } else {
            out.push_str(glyph);
        }
        out.push(' ');
    }

    let indent = if marker.is_some() { MARKER_WIDTH } else { 0 };
    message::write_message(&mut out, message, indent, colour);
    out
}

/// The same line about a named artifact: `✓ /nix/store: missing`.
///
/// The name is printed as it arrived and the rest is read as prose. A caller that has the two
/// separately does not have to hope the shape of the joined line gives them away.
pub fn status_line_about(status: Status, name: &str, message: &str, colour: bool) -> String {
    let marker = status.marker();
    let mut out = String::with_capacity(name.len() + message.len() + 26);

    if let Some((glyph, colour_code)) = marker {
        if colour {
            out.push_str(colour_code);
            out.push_str(glyph);
            out.push_str(RESET);
        } else {
            out.push_str(glyph);
        }
        out.push(' ');
    }

    let indent = if marker.is_some() { MARKER_WIDTH } else { 0 };
    message::write_about(&mut out, name, message, indent, colour);
    out
}

fn print_status(status: Status, message: &str, to_stderr: bool) {
    let colour = if to_stderr {
        stderr_colors()
    } else {
        stdout_colors()
    };
    print_line(&status_line(status, message, colour), to_stderr);
}

pub fn ok(message: impl std::fmt::Display) {
    print_status(Status::Done, &message.to_string(), false);
}

pub fn fail(message: impl std::fmt::Display) {
    print_status(Status::Failed, &message.to_string(), true);
}

/// Reports a failure about a named artifact: the name as it is, and what was found about it.
pub fn fail_about(name: &str, message: &str) {
    print_line(
        &status_line_about(Status::Failed, name, message, stderr_colors()),
        true,
    );
}

/// Reports something that was deliberately left alone, e.g. a package that is already installed.
pub fn skipped(message: impl std::fmt::Display) {
    print_status(Status::LeftAlone, &message.to_string(), true);
}

pub fn warn(message: impl std::fmt::Display) {
    print_status(Status::Warning, &message.to_string(), true);
}

pub fn info(message: impl std::fmt::Display) {
    print_status(Status::Plain, &message.to_string(), true);
}

pub fn header(message: impl std::fmt::Display) {
    let message = message.to_string();
    let mut out = String::with_capacity(message.len() + 8);
    if stderr_colors() {
        out.push_str(BOLD);
    }
    out.push_str(message.trim_end_matches(':'));
    out.push(':');
    if stderr_colors() {
        out.push_str(RESET);
    }
    print_line(&out, true);
}

/// Reports a failure with the causes behind it.
///
/// An error's own message is written by whoever raised it, and it usually interpolates the
/// cause it wraps. A boxed library error is the exception: `reqwest` says that a request failed
/// and leaves *why* — the refused connection, the unresolved host — in its source chain, so a
/// failure printed from the top message alone loses the only line that explains it. The chain is
/// walked here, and a cause is printed only if the message does not already say it.
pub fn fail_error(error: &dyn std::error::Error) {
    print_line(&failure(&error.to_string(), error, stderr_colors()), true);
}

/// Reports a failure in words the caller has already chosen, with the causes behind it.
///
/// The message is the reader's — written where the command is known — and the chain is the
/// library's: the two are printed as one block, and a cause the message already says is left out.
pub fn fail_explained(message: &str, error: &dyn std::error::Error) {
    print_line(&failure(message, error, stderr_colors()), true);
}

pub fn fail_in_detail(message: &str, error: &dyn std::error::Error) {
    print_line(&causes(message, Some(error), stderr_colors()), true);
}

fn failure(message: &str, error: &dyn std::error::Error, colour: bool) -> String {
    causes(message, error.source(), colour)
}

fn causes(message: &str, first: Option<&dyn std::error::Error>, colour: bool) -> String {
    let mut out = status_line(Status::Failed, message, colour);

    let mut source = first;
    let mut printed = 0;
    while let Some(cause) = source.filter(|_| printed < MAX_CAUSES) {
        let text = cause.to_string();
        let text = text.trim();
        if !text.is_empty() && !out.contains(text) {
            out.push('\n');
            for _ in 0..MARKER_WIDTH {
                out.push(' ');
            }
            out.push_str("caused by: ");
            out.push_str(text);
            printed += 1;
        }
        source = cause.source();
    }

    out
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
    fn a_status_line_opens_with_its_marker() {
        assert_eq!(
            status_line(Status::Done, "nothing to install", false),
            "✓ Nothing to install."
        );
    }

    #[test]
    fn a_status_line_colours_the_marker_and_nothing_else() {
        let drawn = status_line(Status::Failed, "it failed", true);
        assert_eq!(drawn, format!("{RED}✗{RESET} It failed."));
    }

    /// `mix doctor` and `mix repair` print one of these per artifact: the name is theirs, the
    /// rest is a measurement.
    #[test]
    fn a_line_about_a_named_artifact_keeps_the_name_it_was_given() {
        assert_eq!(
            status_line_about(Status::Failed, "default profile", "missing", false),
            "✗ default profile: missing."
        );
    }

    #[test]
    fn a_hint_under_a_named_artifact_is_indented_under_the_marker() {
        assert_eq!(
            status_line_about(
                Status::Failed,
                "/nix",
                "exists but is not a directory\nRemove it by hand",
                false
            ),
            "✗ /nix: exists but is not a directory\n  Remove it by hand."
        );
    }

    #[test]
    fn a_hint_under_a_failure_sits_under_the_message() {
        assert_eq!(
            status_line(Status::Failed, "it failed.\nTry again.", false),
            "✗ It failed.\n  Try again."
        );
    }

    #[test]
    fn a_line_without_a_marker_is_not_indented() {
        assert_eq!(
            status_line(
                Status::Plain,
                "root required.\nRe-running with sudo...",
                false
            ),
            "Root required.\nRe-running with sudo..."
        );
    }

    /// The message a library gives is the top of a chain, and the line that explains the failure
    /// is usually further down it.
    #[test]
    fn a_failure_prints_the_cause_behind_it() {
        let error = chain(&[
            "network request failed",
            "error sending request",
            "connection refused (os error 111)",
        ]);

        let printed = failure(&error.to_string(), &error, false);

        assert_eq!(
            printed,
            "✗ Network request failed.\n  \
             caused by: error sending request\n  \
             caused by: connection refused (os error 111)"
        );
    }

    /// Most of the tool's own errors interpolate the cause they wrap, and printing it twice
    /// would be noise.
    #[test]
    fn a_cause_the_message_already_says_is_not_repeated() {
        let error = chain(&[
            "running `nix build`: permission denied",
            "permission denied",
        ]);

        assert_eq!(
            failure(&error.to_string(), &error, false),
            "✗ Running `nix build`: permission denied."
        );
    }

    /// The words are the caller's — written where the command is known — and the chain is still
    /// the library's: a cause those words do not already say belongs under them.
    #[test]
    fn an_explained_failure_keeps_the_causes_under_the_words_chosen_for_it() {
        let error = chain(&[
            "network request failed",
            "connection refused (os error 111)",
        ]);

        let printed = failure(
            "could not fetch the pinned nix archive\ncheck your network connection",
            &error,
            false,
        );

        assert_eq!(
            printed,
            "✗ Could not fetch the pinned nix archive\n  \
             check your network connection.\n  \
             caused by: connection refused (os error 111)"
        );
    }

    #[test]
    fn a_chain_that_never_ends_is_cut_off() {
        let messages: Vec<&'static str> =
            vec!["top", "one", "two", "three", "four", "five", "six", "seven"];
        let chained = chain(&messages);
        let printed = failure(&chained.to_string(), &chained, false);

        assert_eq!(printed.lines().count(), 1 + MAX_CAUSES);
    }

    #[test]
    fn a_warning_is_marked_apart_from_a_failure() {
        assert_eq!(
            status_line(Status::Warning, "your list was reset", false),
            "! Your list was reset."
        );
    }

    #[test]
    fn the_detailed_view_starts_with_the_error_itself() {
        let error = std::io::Error::other("nix build failed: out of disk space");

        let printed = causes("couldn't install ripgrep", Some(&error), false);

        assert_eq!(
            printed,
            "✗ Couldn't install ripgrep.\n  caused by: nix build failed: out of disk space"
        );
    }
}
