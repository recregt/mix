//! Showing a long-running command's output without turning the terminal into a flicker box.

use std::borrow::Cow;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mix_core::{ActivityReporter, BuildProgress, NoopActivity};
use tracing_indicatif::span_ext::IndicatifSpanExt;
use unicode_width::UnicodeWidthChar;

/// How often the live text is redrawn, matching the interval the spinner advances at.
///
/// The two share one draw budget, and asking for more redraws than that budget allows does not
/// buy any: indicatif renders the line in full and then drops it at the rate limiter, and the
/// draw it drops may be the spinner's. Matching the spinner's interval keeps both inside the
/// budget, so every frame of both reaches the terminal. Twelve updates a second is past what
/// there is time to read anyway.
pub const FRAME_INTERVAL_MS: u64 = 80;

/// Longest activity line that is drawn, in terminal columns. The step's own line is trimmed to
/// the real terminal width when it is drawn; this is the upper bound that keeps a runaway line
/// from being carried that far in the first place.
pub const MAX_WIDTH: usize = 72;

const ELLIPSIS: char = '…';

/// Drawn text is dimmed so the step's own label stays the thing being read. Written out rather
/// than styled through a formatter: this is on the frame path, and it is two constants.
const DIM: &str = " \u{1b}[2m";
const UNDIM: &str = "\u{1b}[0m";

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

/// Reduces a raw output line to something that can be drawn on the step's line: no control
/// sequences, and no more than [`MAX_WIDTH`] columns of it.
///
/// Borrows the input in the common case — a plain line that already fits is passed straight
/// through — and never reads further than the line it is going to draw: whatever a flooding
/// build puts on one line, the work here is bounded by the width of the terminal, not by the
/// length of the line.
pub fn display_line(raw: &str) -> Cow<'_, str> {
    // Short and printable: nothing to rewrite. A line's byte length is never below its width in
    // columns, so a line this short cannot be over budget either.
    if raw.len() <= MAX_WIDTH && is_printable(raw.as_bytes()) {
        return Cow::Borrowed(raw);
    }

    // Long, but printable ASCII for as far as it will be drawn: the cut is an index and the copy
    // is one memcpy. Nothing past the cut is examined.
    if raw.len() > MAX_WIDTH && is_printable_ascii(&raw.as_bytes()[..MAX_WIDTH]) {
        let mut out = String::with_capacity(MAX_WIDTH - 1 + ELLIPSIS.len_utf8());
        out.push_str(&raw[..MAX_WIDTH - 1]);
        out.push(ELLIPSIS);
        return Cow::Owned(out);
    }

    Cow::Owned(rewrite(raw))
}

fn is_printable(bytes: &[u8]) -> bool {
    !bytes.iter().any(u8::is_ascii_control)
}

fn is_printable_ascii(bytes: &[u8]) -> bool {
    bytes.is_ascii() && is_printable(bytes)
}

/// Copies out the part of the line that will be drawn, dropping anything that would move the
/// cursor rather than print — escape sequences, and the stray control bytes a build log carries,
/// which would otherwise redraw over the line that is already there.
///
/// Stops as soon as the budget is full, so an escape-laden line costs the width of the terminal
/// rather than its own length. Columns are counted rather than characters: a wide character
/// takes two of them, and counting it as one is how a step line ends up wrapping.
fn rewrite(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_WIDTH * 2));
    let mut columns = 0;
    let mut chars = raw.chars();

    while let Some(c) = chars.next() {
        let c = match c {
            '\u{1b}' => {
                skip_escape(&mut chars);
                continue;
            }
            // A tab still separates two words; every other control byte is dropped.
            '\t' => ' ',
            c if c.is_control() => continue,
            c => c,
        };

        let width = c.width().unwrap_or(0);
        // Leave a column for the ellipsis: there is more line than there is room for it.
        if columns + width > MAX_WIDTH - 1 {
            out.push(ELLIPSIS);
            break;
        }
        out.push(c);
        columns += width;
    }

    out
}

fn skip_escape(chars: &mut std::str::Chars<'_>) {
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

/// Writes a snapshot of what the build is doing, e.g.
/// `building 3/17 · downloading 12/37 · 48.2/91.0 MiB`.
///
/// Counters are cheaper to draw than free-form output: the text is short, bounded, and only the
/// numbers in it change from one frame to the next. It is written into the caller's buffer, and
/// the numbers are written digit by digit, so a drawn frame does no formatting and — once the
/// buffer has been used once — no allocation either.
///
/// What is drawn is also stable from frame to frame: a counter is padded to the width of the
/// total it is counting towards, and both byte counts share the unit of the larger one, so the
/// text keeps its shape as the numbers grow instead of shuffling sideways under the reader.
pub fn write_progress(out: &mut String, progress: &BuildProgress) {
    if progress.builds_expected > 0 || progress.builds_done > 0 {
        write_counter(
            out,
            "building ",
            progress.builds_done,
            progress.builds_expected,
        );
    }

    if progress.downloads_expected > 0 || progress.downloads_done > 0 {
        separate(out);
        write_counter(
            out,
            "downloading ",
            progress.downloads_done,
            progress.downloads_expected,
        );
    }

    if progress.bytes_expected > 0 || progress.bytes_done > 0 {
        separate(out);
        write_bytes(out, progress.bytes_done, progress.bytes_expected);
    }
}

/// [`write_progress`] into a buffer of its own, for a caller that has nothing to reuse.
pub fn render_progress(progress: &BuildProgress) -> String {
    let mut out = String::with_capacity(64);
    write_progress(&mut out, progress);
    out
}

fn separate(out: &mut String) {
    if !out.is_empty() {
        out.push_str(" · ");
    }
}

fn write_counter(out: &mut String, label: &str, done: u64, expected: u64) {
    out.push_str(label);
    // Hold the column the counter ends in steady as it rolls over a power of ten.
    for _ in digits(done)..digits(expected) {
        out.push(' ');
    }
    write_u64(out, done);
    out.push('/');
    write_u64(out, expected);
}

const UNIT: u64 = 1024;
const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

/// Writes both byte counts in the unit of the larger one, e.g. `48.2/91.0 MiB`.
///
/// A shared unit is what makes the pair readable — `900.0 KiB/91.0 MiB` invites the reader to
/// compare two numbers that are not on the same scale — and it is also the cheaper thing to
/// render: the unit is chosen once, and the unit's name is written once.
fn write_bytes(out: &mut String, done: u64, expected: u64) {
    let (divisor, unit) = scale(done.max(expected));
    write_scaled(out, done, divisor);
    out.push('/');
    write_scaled(out, expected, divisor);
    out.push(' ');
    out.push_str(unit);
}

fn scale(bytes: u64) -> (u64, &'static str) {
    let mut divisor = 1u64;
    let mut unit = 0;
    while bytes / divisor >= UNIT && unit + 1 < UNITS.len() {
        divisor *= UNIT;
        unit += 1;
    }
    (divisor, UNITS[unit])
}

/// Writes a byte count with one decimal, in the unit `divisor` stands for.
///
/// Integer maths only: formatting a float is an order of magnitude dearer than dividing, and
/// this runs on every drawn frame.
fn write_scaled(out: &mut String, bytes: u64, divisor: u64) {
    if divisor == 1 {
        write_u64(out, bytes);
        return;
    }

    let mut whole = bytes / divisor;
    let mut tenths = ((bytes % divisor) * 10 + divisor / 2) / divisor;
    if tenths == 10 {
        whole += 1;
        tenths = 0;
    }
    write_u64(out, whole);
    out.push('.');
    out.push((b'0' + tenths as u8) as char);
}

/// Appends a number without going through a formatter: the counters are redrawn many times a
/// second, and `core::fmt` costs more than the digits do.
fn write_u64(out: &mut String, value: u64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut rest = value;
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    // The bytes just written are ASCII digits.
    out.push_str(std::str::from_utf8(&digits[at..]).unwrap_or_default());
}

fn digits(value: u64) -> u32 {
    value.checked_ilog10().unwrap_or(0) + 1
}

/// The frame that is on screen, and the one being built to replace it.
///
/// Both are kept so a frame that renders to what is already drawn can be dropped instead of
/// handed over. That is worth doing because handing one over is not cheap: the progress layer
/// looks the span up, copies the text, re-renders the whole line and trims it to the terminal,
/// and only then finds out whether the terminal is due a write. Comparing two strings is a
/// memcmp. Counters are reported far more often than the text they render to changes — a build
/// step that neither downloads nor finishes anything between two frames draws the same line —
/// so this is the common case rather than a corner one.
///
/// Neither buffer is reallocated after the first few frames: the two are swapped rather than
/// copied, and each is cleared and refilled in place.
pub struct FrameBuffer {
    next: String,
    drawn: String,
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameBuffer {
    pub fn new() -> Self {
        Self {
            next: String::with_capacity(128),
            drawn: String::with_capacity(128),
        }
    }

    /// Builds the next frame and returns it, or `None` if it is the frame already on screen.
    pub fn build(&mut self, fill: impl FnOnce(&mut String)) -> Option<&str> {
        self.next.clear();
        self.next.push_str(DIM);
        fill(&mut self.next);
        self.next.push_str(UNDIM);

        if self.next == self.drawn {
            return None;
        }

        std::mem::swap(&mut self.next, &mut self.drawn);
        Some(&self.drawn)
    }

    /// Forgets what is on screen, for a line that was cleared by someone else.
    pub fn forget(&mut self) {
        self.drawn.clear();
    }
}

struct SpanActivity {
    throttle: Throttle,
    /// Whether nix is reporting counters. Once it is, they own the line: a stray log line must
    /// not fight them for it, and dropping those lines costs an atomic load.
    counting: AtomicBool,
    /// The frame being drawn and the one it replaces. Uncontended in practice: one reader
    /// drains the process's output.
    frame: Mutex<FrameBuffer>,
}

impl SpanActivity {
    fn new() -> Self {
        Self {
            throttle: Throttle::new(),
            counting: AtomicBool::new(false),
            frame: Mutex::new(FrameBuffer::new()),
        }
    }

    /// Builds a frame in the reporter's own buffer and hands it to the step's line, unless it is
    /// the frame that is already there.
    fn draw(&self, fill: impl FnOnce(&mut String)) {
        let Ok(mut frame) = self.frame.lock() else {
            return;
        };
        if let Some(frame) = frame.build(fill) {
            tracing::Span::current().pb_set_message(frame);
        }
    }

    /// Takes the line back, after something else has drawn over it.
    fn cleared(&self) {
        if let Ok(mut frame) = self.frame.lock() {
            frame.forget();
        }
        tracing::Span::current().pb_set_message("");
    }
}

impl ActivityReporter for SpanActivity {
    fn line(&self, line: &str) {
        if self.counting.load(Ordering::Relaxed) || !self.throttle.due() {
            return;
        }
        let line = display_line(line);
        if line.is_empty() {
            return;
        }
        self.draw(|frame| frame.push_str(&line));
    }

    fn progress(&self, progress: &BuildProgress) {
        if progress.is_idle() {
            // Counters that have gone back to nothing would otherwise stay on screen, frozen,
            // for the rest of the step.
            if self.counting.swap(false, Ordering::Relaxed) {
                self.cleared();
            }
            return;
        }
        self.counting.store(true, Ordering::Relaxed);
        if !self.throttle.due() {
            return;
        }
        self.draw(|frame| write_progress(frame, progress));
    }

    fn clear(&self) {
        self.counting.store(false, Ordering::Relaxed);
        self.cleared();
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

    fn build_started(&self, derivation: &str) {
        tracing::debug!("building {derivation}");
    }
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
    fn a_stray_control_byte_is_dropped() {
        assert_eq!(display_line("buil\u{8}ding\u{7}"), "building");
    }

    #[test]
    fn a_tab_is_kept_as_a_space() {
        assert_eq!(display_line("building\tripgrep"), "building ripgrep");
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

    /// Counting wide characters as one column each is how a step line ends up wrapping.
    #[test]
    fn a_line_of_wide_characters_is_truncated_by_column() {
        let line = "パッケージ".repeat(MAX_WIDTH);
        let shown = display_line(&line);
        let columns: usize = shown.chars().map(|c| c.width().unwrap_or(0)).sum();
        assert!(columns <= MAX_WIDTH, "{columns} columns");
        assert!(shown.ends_with(ELLIPSIS));
    }

    /// A line of nothing but escape sequences draws as nothing, however long it is.
    #[test]
    fn a_line_of_escapes_renders_empty() {
        assert_eq!(display_line(&"\u{1b}[2K\u{1b}[1G".repeat(64)), "");
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
            "building  3/17 · downloading 12/37 · 48.2/91.0 MiB"
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
        assert_eq!(render_progress(&progress), "downloading 1/4 · 2.0/8.0 KiB");
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
        assert_eq!(render_progress(&progress), "12/900 B");
    }

    /// 48.185… MiB: truncating would report a tenth less than it should.
    #[test]
    fn a_byte_count_is_rounded_rather_than_truncated() {
        let progress = BuildProgress {
            bytes_done: 50_525_798,
            bytes_expected: 50_525_798,
            ..BuildProgress::default()
        };
        assert_eq!(render_progress(&progress), "48.2/48.2 MiB");
    }

    #[test]
    fn large_byte_counts_reach_gibibytes() {
        let progress = BuildProgress {
            bytes_done: 3_221_225_472,
            bytes_expected: 6_442_450_944,
            ..BuildProgress::default()
        };
        assert_eq!(render_progress(&progress), "3.0/6.0 GiB");
    }

    /// The counters are read while they move, so a digit rolling over must not shift the text
    /// that follows it sideways.
    #[test]
    fn a_counter_keeps_its_width_as_it_rolls_over() {
        let at = |done| {
            render_progress(&BuildProgress {
                builds_done: done,
                builds_expected: 120,
                ..BuildProgress::default()
            })
        };
        assert_eq!(at(9).len(), at(10).len());
        assert_eq!(at(99).len(), at(100).len());
        assert_eq!(at(9), "building   9/120");
    }

    /// Two counts on different scales are not a comparison a reader should have to do in their
    /// head, and the shared unit is what keeps the pair the same width as it grows.
    #[test]
    fn both_byte_counts_share_the_unit_of_the_larger_one() {
        let progress = BuildProgress {
            bytes_done: 900 * 1024,
            bytes_expected: 91 * 1024 * 1024,
            ..BuildProgress::default()
        };
        assert_eq!(render_progress(&progress), "0.9/91.0 MiB");
    }

    #[test]
    fn a_reused_buffer_renders_what_a_fresh_one_does() {
        let mut buffer = String::new();
        write_progress(&mut buffer, &progress());
        buffer.clear();
        write_progress(&mut buffer, &progress());
        assert_eq!(buffer, render_progress(&progress()));
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
    fn a_frame_that_is_already_on_screen_is_not_drawn_again() {
        let mut frames = FrameBuffer::new();
        assert!(
            frames
                .build(|out| write_progress(out, &progress()))
                .is_some()
        );
        assert!(
            frames
                .build(|out| write_progress(out, &progress()))
                .is_none()
        );
    }

    #[test]
    fn a_frame_that_changed_is_drawn() {
        let mut frames = FrameBuffer::new();
        frames.build(|out| write_progress(out, &progress()));

        let moved = BuildProgress {
            builds_done: 4,
            ..progress()
        };
        let drawn = frames.build(|out| write_progress(out, &moved));
        assert!(drawn.is_some_and(|frame| frame.contains("building  4/17")));
    }

    /// The line is cleared behind the reporter's back when the counters go idle, so the frame it
    /// thinks is on screen has to go with it.
    #[test]
    fn a_frame_is_drawn_again_after_the_line_is_cleared() {
        let mut frames = FrameBuffer::new();
        frames.build(|out| write_progress(out, &progress()));
        frames.forget();
        assert!(
            frames
                .build(|out| write_progress(out, &progress()))
                .is_some()
        );
    }

    #[test]
    fn a_drawn_frame_is_dimmed() {
        let mut frames = FrameBuffer::new();
        let drawn = frames.build(|out| out.push_str("copying path")).unwrap();
        assert_eq!(drawn, format!("{DIM}copying path{UNDIM}"));
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
