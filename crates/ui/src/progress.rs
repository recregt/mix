use std::sync::Arc;
use std::time::Duration;

use indicatif::ProgressStyle;
use mix_core::{DownloadProgress, StepObserver};

use crate::live::LiveLine;
use owo_colors::OwoColorize;
use owo_colors::colors::{Green, Red};
use tracing::field::{Field, Visit};
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_indicatif::{IndicatifLayer, TickSettings};
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry};

/// A download knows its total, so it gets the one thing a spinner cannot give: a bar that says
/// how much of the wait is left. `{wide_bar}` takes exactly the columns the rest of the line
/// leaves free, so the line fits whatever the terminal's width happens to be.
const DOWNLOAD_STYLE: &str = "{span_child_prefix}{spinner}{span_fields} {wide_bar:.cyan/dim} {bytes}/{total_bytes} ({binary_bytes_per_sec})";

/// `{wide_msg}` carries the live output of whatever the step is running. It stays empty until a
/// command reports its first line, so a quiet step renders exactly as before, and the width it
/// is given is measured against the real terminal: the live text is trimmed to the columns that
/// are actually free, so a narrow terminal or a long step name can never wrap the line and set
/// the whole display scrolling.
const STEP_STYLE: &str = "{span_child_prefix}{spinner}{span_fields}{wide_msg}";

/// Filled, leading edge, empty. A solid bar reads as one object at a glance, where a bar of
/// blocks reads as a row of them.
const PROGRESS_CHARS: &str = "━╸━";

const OWN_CRATES: [&str; 5] = ["mix_core", "mix_exec", "mix_shell", "mix_cli", "mix_ui"];

const ANCHORS: &str = "mix_shell::logs";

/// The spinner's frames: a three-dot arc sweeping once around the braille cell.
///
/// Every frame lights exactly three dots and is one column wide, so the spinner turns at a
/// constant weight and never shifts the text beside it, and no frame repeats its predecessor, so
/// it never appears to stall mid-turn. Eight frames is one turn, which is short enough to read
/// as a rotation rather than as a pattern.
const TICK_FRAMES: [&str; 8] = ["⠋", "⠙", "⠸", "⢰", "⣠", "⣄", "⡆", "⠇"];

/// The glyph a finished line keeps. The same markers every other finished line in the tool is
/// printed with, so a step that is done reads the same whether it was drawn or printed.
const DONE: &str = "✓";
const FAILED: &str = "✗";

/// How long a spinner frame is shown. Eight frames at this interval is a turn in 640 ms.
///
/// This is also one half of the draw budget: the spinner asks for a redraw this often, and the
/// live text asks for one every [`FRAME_INTERVAL_MS`](crate::activity::FRAME_INTERVAL_MS). The
/// two together have to fit in [`TERM_DRAW_HZ`], or indicatif starts dropping draws — and a
/// dropped draw is a spinner frame that never reaches the terminal.
const SPINNER_TICK_MS: u64 = 80;

/// Redraws allowed per second. Above what the spinner and the live text together ask for, so
/// neither is dropped; a draw that is dropped is paid for in full anyway, since the line is
/// rendered before the rate limiter is consulted.
const TERM_DRAW_HZ: u8 = 25;

/// Wraps the glyph a finished line keeps in its own colour.
///
/// `{spinner:.cyan}` colours the whole slot, so the glyph has to carry its colour itself to come
/// out in anything else; the inner sequence is the one the terminal applies last. Built once per
/// style rather than per frame, and left plain when colour is off.
fn finished(glyph: &str, failed: bool) -> String {
    if !crate::stderr_colors() {
        return glyph.to_string();
    }
    if failed {
        format!("{}", glyph.fg::<Red>())
    } else {
        format!("{}", glyph.fg::<Green>())
    }
}

/// The style of a line that is still running, and of the marker it keeps once it is not.
fn spinner_style(template: &str, finished: &str) -> ProgressStyle {
    let colors = crate::stderr_colors();
    let paint = |glyph: &str| {
        if colors {
            format!("\u{1b}[36m{glyph}\u{1b}[0m ")
        } else {
            format!("{glyph} ")
        }
    };
    let mut frames: Vec<String> = TICK_FRAMES.iter().map(|frame| paint(frame)).collect();
    frames.push(paint(finished));
    let frames: Vec<&str> = frames.iter().map(String::as_str).collect();

    ProgressStyle::with_template(template)
        .expect("progress bar template is valid")
        .tick_strings(&frames)
}

/// The style of a step's line: a spinner, the step's name, and whatever the step is running.
pub fn step_style() -> ProgressStyle {
    spinner_style(STEP_STYLE, &finished(DONE, false))
}

/// The same line, for a step that did not get there. The glyph a finished line keeps is the
/// spinner's last frame, so swapping the style is what turns a `✓` into a `✗`.
fn failed_step_style() -> ProgressStyle {
    spinner_style(STEP_STYLE, &finished(FAILED, true))
}

fn download_style() -> ProgressStyle {
    spinner_style(DOWNLOAD_STYLE, &finished(DONE, false)).progress_chars(PROGRESS_CHARS)
}

pub fn verbosity_level(verbosity: u8) -> tracing::Level {
    match verbosity {
        0 => tracing::Level::WARN,
        1 => tracing::Level::INFO,
        2 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    }
}

fn level_filter(verbosity: u8) -> LevelFilter {
    LevelFilter::from_level(verbosity_level(verbosity))
}

struct NameOnlyFields;

impl<'writer> FormatFields<'writer> for NameOnlyFields {
    fn format_fields<R: RecordFields>(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        struct NameVisitor<'a, 'w> {
            writer: &'a mut Writer<'w>,
        }

        impl Visit for NameVisitor<'_, '_> {
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "name" {
                    let _ = write!(self.writer, "{value}");
                }
            }

            fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
        }

        fields.record(&mut NameVisitor {
            writer: &mut writer,
        });
        Ok(())
    }
}

fn own_crates_only() -> Targets {
    own_crates_at(LevelFilter::TRACE, LevelFilter::OFF)
}

/// What `-v` turns up, and what it deliberately does not.
///
/// Verbosity is a request for more of *this* tool's reasoning, not for its dependencies': a
/// blanket level filter meant `-vv` also turned on every debug line `hyper`, `rustls` and
/// `tokio` emit, which is thousands of lines around the one being looked for. Their warnings
/// are still shown, at the level they were shown at before.
fn own_crates_at(level: LevelFilter, default: LevelFilter) -> Targets {
    OWN_CRATES
        .iter()
        .fold(Targets::new(), |targets, name| {
            targets.with_target(*name, level)
        })
        .with_target(ANCHORS, LevelFilter::OFF)
        .with_default(default)
}

fn log_filter(verbosity: u8) -> Targets {
    own_crates_at(level_filter(verbosity), LevelFilter::WARN)
}

pub(crate) struct IndicatifDownloadProgress {
    live: Option<Arc<LiveLine>>,
}

impl IndicatifDownloadProgress {
    pub(crate) fn new(live: Option<Arc<LiveLine>>) -> Self {
        Self { live }
    }
}

impl DownloadProgress for IndicatifDownloadProgress {
    fn set_total(&self, total: u64) {
        let span = tracing::Span::current();
        span.pb_set_style(&download_style());
        span.pb_set_length(total);
    }

    fn add(&self, delta: u64) {
        tracing::Span::current().pb_inc(delta);
        if let Some(live) = &self.live {
            live.received();
        }
    }
}

pub(crate) struct IndicatifStepObserver {
    live: Option<Arc<LiveLine>>,
    keep_line: bool,
}

impl IndicatifStepObserver {
    pub(crate) fn new(live: Option<Arc<LiveLine>>, keep_line: bool) -> Self {
        Self { live, keep_line }
    }
}

impl StepObserver for IndicatifStepObserver {
    fn on_step_span(&self, span: &tracing::Span) {
        // Persist the line instead of clearing it: `finish_using_style` always forces an
        // immediate draw regardless of the redraw rate limiter, so even a step that completes
        // in a millisecond is guaranteed to render before its line disappears.
        if self.keep_line {
            span.pb_set_finish_message("");
        }
        if let Some(live) = &self.live {
            live.opened(span);
        }
    }

    fn on_step_closed(&self, span: &tracing::Span, failed: bool) {
        if failed {
            span.pb_set_style(&failed_step_style());
        }
        if let Some(live) = &self.live {
            live.closed(span);
        }
    }
}

/// Sets up the output for the whole process.
///
/// With `progress` off nothing is drawn in place: no spinners, no live output, just log lines on
/// stderr. That is what a script or a CI job wants even when it happens to own a terminal.
pub fn init_tracing<L>(verbosity: u8, progress: bool, terminal: bool, logs: L)
where
    L: Layer<Registry> + Send + Sync + 'static,
{
    crate::set_progress_enabled(progress);
    let registry = tracing_subscriber::registry().with(logs);

    if !terminal {
        registry.init();
        return;
    }

    if !progress {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .without_time()
                    .with_target(false)
                    .with_filter(log_filter(verbosity)),
            )
            .init();
        return;
    }

    let indicatif_layer = IndicatifLayer::new()
        .with_span_field_formatter(NameOnlyFields)
        .with_progress_style(step_style())
        .with_tick_settings(TickSettings {
            term_draw_hz: TERM_DRAW_HZ,
            default_tick_interval: Some(Duration::from_millis(SPINNER_TICK_MS)),
            footer_tick_interval: None,
            ..Default::default()
        });

    registry
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(indicatif_layer.get_stderr_writer())
                .without_time()
                .with_target(false)
                .with_filter(log_filter(verbosity)),
        )
        .with(indicatif_layer.with_filter(own_crates_only()))
        .init();
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use indicatif::{ProgressBar, ProgressDrawTarget, TermLike};

    use super::*;

    #[test]
    fn verbose_output_includes_the_commands_every_crate_runs() {
        let targets = own_crates_at(LevelFilter::DEBUG, LevelFilter::OFF);

        for name in ["mix_core", "mix_exec", "mix_shell", "mix_cli", "mix_ui"] {
            assert!(
                targets.would_enable(name, &tracing::Level::DEBUG),
                "{name} is filtered out of verbose output"
            );
        }
        assert!(!targets.would_enable("hyper", &tracing::Level::DEBUG));
    }

    #[test]
    fn the_spans_that_tie_logs_to_a_node_are_never_drawn_or_printed() {
        for targets in [own_crates_only(), log_filter(3)] {
            assert!(!targets.would_enable(ANCHORS, &tracing::Level::ERROR));
            assert!(targets.would_enable("mix_shell::ops", &tracing::Level::TRACE));
        }
    }

    #[derive(Clone, Debug)]
    struct RecordingTerm(Arc<Mutex<Vec<String>>>);

    impl TermLike for RecordingTerm {
        fn width(&self) -> u16 {
            80
        }

        fn move_cursor_up(&self, _n: usize) -> std::io::Result<()> {
            Ok(())
        }

        fn move_cursor_down(&self, _n: usize) -> std::io::Result<()> {
            Ok(())
        }

        fn move_cursor_right(&self, _n: usize) -> std::io::Result<()> {
            Ok(())
        }

        fn move_cursor_left(&self, _n: usize) -> std::io::Result<()> {
            Ok(())
        }

        fn write_line(&self, s: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(s.to_string());
            Ok(())
        }

        fn write_str(&self, s: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(s.to_string());
            Ok(())
        }

        fn clear_line(&self) -> std::io::Result<()> {
            Ok(())
        }

        fn flush(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    const TERM_WIDTH: usize = 80;

    fn render(style: ProgressStyle, message: &str) -> String {
        render_lines(style, message, None).join("")
    }

    /// Every line the bar drew, one entry per write the terminal saw.
    fn render_lines(style: ProgressStyle, message: &str, length: Option<u64>) -> Vec<String> {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let bar = ProgressBar::with_draw_target(
            length,
            ProgressDrawTarget::term_like(Box::new(RecordingTerm(Arc::clone(&recorded)))),
        )
        .with_style(style.progress_chars(PROGRESS_CHARS));
        bar.set_message(message.to_string());
        bar.tick();
        bar.finish_and_clear();

        let recorded = recorded.lock().unwrap();
        recorded.clone()
    }

    /// What a line keeps once the step it was drawn for is over.
    fn finished_line(style: ProgressStyle) -> String {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let bar = ProgressBar::with_draw_target(
            None,
            ProgressDrawTarget::term_like(Box::new(RecordingTerm(Arc::clone(&recorded)))),
        )
        .with_style(style);
        bar.tick();
        bar.finish_with_message("");

        let recorded = recorded.lock().unwrap();
        recorded.join("")
    }

    /// The widest line the bar drew, in columns.
    fn drawn_columns(lines: &[String]) -> usize {
        lines.iter().map(|line| columns(line)).max().unwrap_or(0)
    }

    /// Columns the terminal would need for a drawn line, escape sequences excluded.
    fn columns(line: &str) -> usize {
        console::measure_text_width(line)
    }

    #[test]
    fn the_step_style_draws_the_live_message() {
        assert!(render(step_style(), "copying path").contains("copying path"));
    }

    #[test]
    fn the_step_style_adds_nothing_until_a_line_is_reported() {
        assert_eq!(
            render(step_style(), ""),
            render(
                spinner_style(
                    "{span_child_prefix}{spinner}{span_fields}",
                    &finished(DONE, false)
                ),
                ""
            )
        );
    }

    /// A step line that outgrows the terminal wraps, and a wrapped line is redrawn as two: the
    /// display starts scrolling instead of staying put.
    #[test]
    fn a_long_line_is_trimmed_to_the_terminal_width() {
        let drawn = render_lines(step_style(), &format!(" {}", "x".repeat(200)), None);
        let widest = drawn_columns(&drawn);
        assert!(widest <= TERM_WIDTH, "{widest} columns");
    }

    /// The live text is dimmed, so the trimming has to keep the escape sequences it is wrapped
    /// in rather than cutting through one.
    #[test]
    fn trimming_keeps_the_styling_of_the_text_it_trims() {
        let drawn = render_lines(
            step_style(),
            &format!(" \u{1b}[2m{}\u{1b}[0m", "x".repeat(200)),
            None,
        );
        assert!(drawn_columns(&drawn) <= TERM_WIDTH);
        assert!(drawn.iter().any(|line| line.ends_with("\u{1b}[0m")));
    }

    #[test]
    fn the_download_style_fits_the_terminal_width() {
        let drawn = render_lines(download_style(), "", Some(91 * 1024 * 1024));
        let widest = drawn_columns(&drawn);
        assert!(widest <= TERM_WIDTH, "{widest} columns");
        assert!(drawn.iter().any(|line| line.contains('━')));
    }

    /// A frame wider than one column would move the text beside it every time the spinner
    /// turned, which is the one thing a line that is redrawn in place must not do.
    #[test]
    fn every_frame_of_the_spinner_is_one_column_wide() {
        for frame in TICK_FRAMES {
            assert_eq!(columns(frame), 1, "{frame:?}");
        }
        assert_eq!(columns(DONE), 1);
        assert_eq!(columns(FAILED), 1);
    }

    /// A frame drawn twice in a row reads as the spinner having stopped.
    #[test]
    fn no_frame_of_the_spinner_repeats() {
        let mut seen = TICK_FRAMES.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), TICK_FRAMES.len());
    }

    /// The spinner and the live text each ask for a redraw on their own schedule, against one
    /// budget. Over it, indicatif renders a line and then throws it away — and the frame it
    /// throws away is as likely to be the spinner's as the text's.
    #[test]
    fn the_spinner_and_the_live_text_fit_the_draw_budget() {
        let asked = 1000 / SPINNER_TICK_MS + 1000 / crate::activity::FRAME_INTERVAL_MS;
        assert!(asked <= u64::from(TERM_DRAW_HZ), "{asked} draws a second");
    }

    /// `-vv` is a request for this tool's own reasoning; a dependency's debug stream is noise
    /// around the line being looked for.
    #[test]
    fn verbosity_turns_up_this_tool_and_not_its_dependencies() {
        let filter = log_filter(2);

        assert!(filter.would_enable("mix_shell", &tracing::Level::DEBUG));
        assert!(filter.would_enable("mix_shell::ops::bootstrap", &tracing::Level::DEBUG));
        assert!(!filter.would_enable("hyper::client", &tracing::Level::DEBUG));
    }

    /// A dependency that has something to warn about is still worth hearing from.
    #[test]
    fn a_dependency_keeps_the_level_it_was_always_heard_at() {
        for verbosity in [0, 1, 2, 3] {
            let filter = log_filter(verbosity);
            assert!(filter.would_enable("rustls::client", &tracing::Level::WARN));
            assert!(!filter.would_enable("rustls::client", &tracing::Level::INFO));
        }
    }

    #[test]
    fn a_finished_step_keeps_the_marker_of_a_step_that_worked() {
        assert!(finished_line(step_style()).contains(DONE));
    }

    /// The marker a finished line keeps is the spinner's last frame, so a step that failed has
    /// to be given a different one before its line is finished.
    #[test]
    fn a_failed_step_keeps_the_marker_of_a_step_that_did_not() {
        let drawn = finished_line(failed_step_style());
        assert!(drawn.contains(FAILED), "{drawn:?}");
        assert!(!drawn.contains(DONE), "{drawn:?}");
    }
}
