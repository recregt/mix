//! Showing a long-running command's output without turning the terminal into a flicker box.

use std::borrow::Cow;
use std::fmt::Write;
use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use mix_core::{ActivityReporter, BuildProgress, NoopActivity};
use owo_colors::OwoColorize;
use tracing_indicatif::span_ext::IndicatifSpanExt;

/// One frame per 50 ms, matching the rate the progress bars are redrawn at: drawing faster only
/// costs work the terminal throws away.
const FRAME_INTERVAL_MS: u64 = 50;

/// Longest activity line that is drawn, in characters. Anything past this is noise on a
/// standard-width terminal, and truncating here keeps the step on a single line.
pub const MAX_WIDTH: usize = 72;

const ELLIPSIS: char = '…';

/// Sentinel for "nothing drawn yet", so the very first line shows up without waiting a frame.
const NEVER: u64 = u64::MAX;

/// Drops frames that are not due yet, so a process that floods its output costs one clock read
/// per line instead of a redraw.
pub struct Throttle {
    started: Instant,
    interval_ms: u64,
    last_frame_ms: AtomicU64,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

impl Throttle {
    pub fn new() -> Self {
        Self::with_interval_ms(FRAME_INTERVAL_MS)
    }

    pub fn with_interval_ms(interval_ms: u64) -> Self {
        Self {
            started: Instant::now(),
            interval_ms,
            last_frame_ms: AtomicU64::new(NEVER),
        }
    }

    /// Whether a frame should be drawn now. At most one caller is told yes per interval.
    pub fn due(&self) -> bool {
        let now = self.started.elapsed().as_millis() as u64;
        let last = self.last_frame_ms.load(Ordering::Relaxed);
        if last != NEVER && now.saturating_sub(last) < self.interval_ms {
            return false;
        }
        self.last_frame_ms
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }
}

/// Reduces a raw output line to something that fits on the step's line.
///
/// Borrows the input in the common case: a plain line that already fits is passed straight
/// through, and only escape sequences or an overlong line force a copy.
pub fn display_line(raw: &str) -> Cow<'_, str> {
    match strip_ansi(raw) {
        Cow::Borrowed(plain) => truncate(plain),
        Cow::Owned(stripped) => Cow::Owned(truncate(&stripped).into_owned()),
    }
}

fn truncate(line: &str) -> Cow<'_, str> {
    // Fast path: a byte length within the budget cannot exceed it in characters.
    if line.len() <= MAX_WIDTH {
        return Cow::Borrowed(line);
    }
    match line.char_indices().nth(MAX_WIDTH - 1) {
        None => Cow::Borrowed(line),
        Some((end, _)) => {
            let mut out = String::with_capacity(end + ELLIPSIS.len_utf8());
            out.push_str(&line[..end]);
            out.push(ELLIPSIS);
            Cow::Owned(out)
        }
    }
}

fn strip_ansi(raw: &str) -> Cow<'_, str> {
    if !raw.as_bytes().contains(&0x1b) {
        return Cow::Borrowed(raw);
    }

    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then a final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if matches!(c, '@'..='~') {
                        break;
                    }
                }
            }
            // OSC: terminated by BEL or ESC \.
            Some(']') => {
                for c in chars.by_ref() {
                    if c == '\u{7}' || c == '\u{1b}' {
                        break;
                    }
                }
            }
            // Any other two-byte escape: both bytes are already consumed.
            _ => {}
        }
    }
    Cow::Owned(out)
}

/// Renders a snapshot of what the build is doing, e.g.
/// `building 3/17 · downloading 12/37 · 48.2 MiB/91.0 MiB`.
///
/// Counters are cheaper to draw than free-form output: the text is short, bounded, and only the
/// numbers in it change from one frame to the next.
pub fn render_progress(progress: &BuildProgress) -> String {
    let mut out = String::with_capacity(64);

    if progress.builds_expected > 0 || progress.builds_done > 0 {
        let _ = write!(
            out,
            "building {}/{}",
            progress.builds_done, progress.builds_expected
        );
    }

    if progress.downloads_expected > 0 || progress.downloads_done > 0 {
        separate(&mut out);
        let _ = write!(
            out,
            "downloading {}/{}",
            progress.downloads_done, progress.downloads_expected
        );
    }

    if progress.bytes_expected > 0 || progress.bytes_done > 0 {
        separate(&mut out);
        write_bytes(&mut out, progress.bytes_done);
        out.push('/');
        write_bytes(&mut out, progress.bytes_expected);
    }

    out
}

fn separate(out: &mut String) {
    if !out.is_empty() {
        out.push_str(" · ");
    }
}

/// Writes a byte count with one decimal, e.g. `48.2 MiB`.
///
/// Integer maths only: formatting a float is an order of magnitude dearer than dividing, and
/// this runs on every drawn frame.
fn write_bytes(out: &mut String, bytes: u64) {
    const UNIT: u64 = 1024;
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    let mut divisor = 1u64;
    let mut unit = 0;
    while bytes / divisor >= UNIT && unit + 1 < UNITS.len() {
        divisor *= UNIT;
        unit += 1;
    }

    let _ = if unit == 0 {
        write!(out, "{bytes} B")
    } else {
        let mut whole = bytes / divisor;
        let mut tenths = ((bytes % divisor) * 10 + divisor / 2) / divisor;
        if tenths == 10 {
            whole += 1;
            tenths = 0;
        }
        write!(out, "{whole}.{tenths} {}", UNITS[unit])
    };
}

struct SpanActivity {
    throttle: Throttle,
    /// Whether nix is reporting counters. Once it is, they own the line: a stray log line must
    /// not fight them for it, and dropping those lines costs an atomic load.
    counting: AtomicBool,
}

impl SpanActivity {
    fn new() -> Self {
        Self {
            throttle: Throttle::new(),
            counting: AtomicBool::new(false),
        }
    }
}

impl ActivityReporter for SpanActivity {
    fn line(&self, line: &str) {
        if self.counting.load(Ordering::Relaxed) || !self.throttle.due() {
            return;
        }
        let line = display_line(line);
        tracing::Span::current().pb_set_message(&format!(" {}", line.dimmed()));
    }

    fn progress(&self, progress: &BuildProgress) {
        if progress.is_idle() {
            self.counting.store(false, Ordering::Relaxed);
            return;
        }
        self.counting.store(true, Ordering::Relaxed);
        if !self.throttle.due() {
            return;
        }
        let rendered = render_progress(progress);
        tracing::Span::current().pb_set_message(&format!(" {}", rendered.dimmed()));
    }

    fn clear(&self) {
        self.counting.store(false, Ordering::Relaxed);
        tracing::Span::current().pb_set_message("");
    }
}

/// Forwards the output to the log instead of drawing it, for pipes and CI logs where a redrawn
/// line would just be noise. Costs nothing until `-vv` turns the level on.
struct LoggedActivity;

impl ActivityReporter for LoggedActivity {
    fn line(&self, line: &str) {
        tracing::debug!("{line}");
    }

    fn progress(&self, _progress: &BuildProgress) {}

    fn clear(&self) {}
}

/// The reporter to hand to a long-running command, chosen once for the process.
///
/// Nothing is drawn unless progress was left on and there is a terminal watching: a script that
/// asked for plain output pays nothing per line.
pub fn activity_reporter() -> Arc<dyn ActivityReporter> {
    if crate::progress_enabled() && std::io::stderr().is_terminal() {
        Arc::new(SpanActivity::new())
    } else if tracing::enabled!(tracing::Level::DEBUG) {
        Arc::new(LoggedActivity)
    } else {
        Arc::new(NoopActivity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_line_is_passed_through_without_copying() {
        let line = "copying path '/nix/store/abc-git-2.45.0'";
        assert!(matches!(display_line(line), Cow::Borrowed(_)));
        assert_eq!(display_line(line), line);
    }

    #[test]
    fn colour_sequences_are_stripped() {
        assert_eq!(
            display_line("\u{1b}[32mbuilding\u{1b}[0m '/nix/store/abc'"),
            "building '/nix/store/abc'"
        );
    }

    #[test]
    fn a_cursor_sequence_is_stripped() {
        assert_eq!(display_line("\u{1b}[2K\u{1b}[1Gfetching"), "fetching");
    }

    #[test]
    fn an_operating_system_command_is_stripped() {
        assert_eq!(display_line("\u{1b}]0;title\u{7}building"), "building");
    }

    #[test]
    fn a_lone_escape_does_not_swallow_the_rest_of_the_line() {
        assert_eq!(display_line("a\u{1b}Zb"), "ab");
    }

    #[test]
    fn a_long_line_is_truncated_with_an_ellipsis() {
        let line = "x".repeat(MAX_WIDTH * 2);
        let shown = display_line(&line);
        assert_eq!(shown.chars().count(), MAX_WIDTH);
        assert!(shown.ends_with(ELLIPSIS));
    }

    #[test]
    fn a_line_exactly_at_the_budget_is_left_alone() {
        let line = "x".repeat(MAX_WIDTH);
        assert_eq!(display_line(&line), line);
    }

    #[test]
    fn truncation_does_not_split_a_multibyte_character() {
        let line = "é".repeat(MAX_WIDTH * 2);
        let shown = display_line(&line);
        assert_eq!(shown.chars().count(), MAX_WIDTH);
    }

    #[test]
    fn a_stripped_line_is_still_truncated() {
        let line = format!("\u{1b}[32m{}", "y".repeat(MAX_WIDTH * 2));
        assert_eq!(display_line(&line).chars().count(), MAX_WIDTH);
    }

    #[test]
    fn the_first_frame_is_always_due() {
        assert!(Throttle::new().due());
    }

    #[test]
    fn a_burst_of_lines_draws_one_frame_and_drops_the_rest() {
        let throttle = Throttle::with_interval_ms(60_000);
        let drawn = (0..100_000).filter(|_| throttle.due()).count();
        assert_eq!(drawn, 1);
    }

    #[test]
    fn a_frame_becomes_due_again_after_the_interval() {
        let throttle = Throttle::new();
        assert!(throttle.due());
        std::thread::sleep(std::time::Duration::from_millis(FRAME_INTERVAL_MS + 10));
        assert!(throttle.due());
    }

    #[test]
    fn reporting_a_line_without_a_progress_bar_is_harmless() {
        let reporter = SpanActivity::new();
        reporter.line("no subscriber is installed");
        reporter.progress(&BuildProgress {
            builds_done: 1,
            builds_expected: 2,
            ..BuildProgress::default()
        });
        reporter.clear();
    }

    fn progress() -> BuildProgress {
        BuildProgress {
            builds_done: 3,
            builds_expected: 17,
            builds_running: 1,
            downloads_done: 12,
            downloads_expected: 37,
            downloads_running: 2,
            bytes_done: 50_525_798,
            bytes_expected: 95_420_416,
        }
    }

    #[test]
    fn a_full_snapshot_renders_builds_downloads_and_bytes() {
        assert_eq!(
            render_progress(&progress()),
            "building 3/17 · downloading 12/37 · 48.2 MiB/91.0 MiB"
        );
    }

    #[test]
    fn a_download_only_snapshot_leaves_the_build_counter_out() {
        let progress = BuildProgress {
            downloads_done: 1,
            downloads_expected: 4,
            bytes_done: 2_048,
            bytes_expected: 8_192,
            ..BuildProgress::default()
        };
        assert_eq!(
            render_progress(&progress),
            "downloading 1/4 · 2.0 KiB/8.0 KiB"
        );
    }

    #[test]
    fn a_build_only_snapshot_is_just_the_build_counter() {
        let progress = BuildProgress {
            builds_done: 1,
            builds_expected: 2,
            ..BuildProgress::default()
        };
        assert_eq!(render_progress(&progress), "building 1/2");
    }

    #[test]
    fn small_byte_counts_stay_in_bytes() {
        let progress = BuildProgress {
            bytes_done: 12,
            bytes_expected: 900,
            ..BuildProgress::default()
        };
        assert_eq!(render_progress(&progress), "12 B/900 B");
    }

    /// 48.185… MiB: truncating would report a tenth less than it should.
    #[test]
    fn a_byte_count_is_rounded_rather_than_truncated() {
        let progress = BuildProgress {
            bytes_done: 50_525_798,
            bytes_expected: 50_525_798,
            ..BuildProgress::default()
        };
        assert_eq!(render_progress(&progress), "48.2 MiB/48.2 MiB");
    }

    #[test]
    fn large_byte_counts_reach_gibibytes() {
        let progress = BuildProgress {
            bytes_done: 3_221_225_472,
            bytes_expected: 6_442_450_944,
            ..BuildProgress::default()
        };
        assert_eq!(render_progress(&progress), "3.0 GiB/6.0 GiB");
    }

    #[test]
    fn an_idle_snapshot_renders_nothing() {
        assert!(render_progress(&BuildProgress::default()).is_empty());
    }

    /// Nothing nix reports comes close to these, and even then the counters stay on one line.
    #[test]
    fn the_rendered_counters_fit_on_the_step_line() {
        let progress = BuildProgress {
            builds_done: 999_999,
            builds_expected: 999_999,
            downloads_done: 999_999,
            downloads_expected: 999_999,
            bytes_done: 99 * 1024 * 1024 * 1024,
            bytes_expected: 99 * 1024 * 1024 * 1024,
            ..progress()
        };
        assert!(
            render_progress(&progress).chars().count() <= MAX_WIDTH,
            "{}",
            render_progress(&progress)
        );
    }

    #[test]
    fn counters_take_the_line_over_from_free_form_output() {
        let reporter = SpanActivity::new();
        reporter.progress(&progress());
        assert!(reporter.counting.load(Ordering::Relaxed));

        // Cheap enough to be safe under a flood: a dropped line is one atomic load.
        reporter.line("copying path");
        assert!(reporter.counting.load(Ordering::Relaxed));
    }

    #[test]
    fn an_idle_snapshot_hands_the_line_back_to_free_form_output() {
        let reporter = SpanActivity::new();
        reporter.progress(&progress());
        reporter.progress(&BuildProgress::default());
        assert!(!reporter.counting.load(Ordering::Relaxed));
    }
}
