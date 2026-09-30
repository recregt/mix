use std::borrow::Cow;

use mix_core::BuildProgress;

pub fn printable(raw: &str) -> Cow<'_, str> {
    if !raw.bytes().any(|byte| byte.is_ascii_control()) {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => skip_escape(&mut chars),
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
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
/// `building 3/17, downloading 12/37, 48.2/91.0 MiB`.
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
        out.push_str(", ");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_line_is_passed_through_without_copying() {
        let line = "copying path '/nix/store/abc-git-2.45.0'";
        assert!(matches!(printable(line), Cow::Borrowed(_)));
        assert_eq!(printable(line), line);
    }

    #[test]
    fn colour_sequences_are_stripped() {
        assert_eq!(
            printable("\u{1b}[32mbuilding\u{1b}[0m '/nix/store/abc'"),
            "building '/nix/store/abc'"
        );
    }

    #[test]
    fn a_cursor_sequence_is_stripped() {
        assert_eq!(printable("\u{1b}[2K\u{1b}[1Gfetching"), "fetching");
    }

    #[test]
    fn an_operating_system_command_is_stripped() {
        assert_eq!(printable("\u{1b}]0;title\u{7}building"), "building");
    }

    #[test]
    fn a_lone_escape_does_not_swallow_the_rest_of_the_line() {
        assert_eq!(printable("a\u{1b}Zb"), "ab");
    }

    #[test]
    fn a_stray_control_byte_is_dropped() {
        assert_eq!(printable("buil\u{8}ding\u{7}"), "building");
    }

    #[test]
    fn a_tab_is_kept_as_a_space() {
        assert_eq!(printable("building\tripgrep"), "building ripgrep");
    }

    #[test]
    fn a_long_line_is_printed_whole() {
        let line = format!("\u{1b}[1m{}", "x".repeat(500));
        assert_eq!(printable(&line).len(), 500);
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
            "building  3/17, downloading 12/37, 48.2/91.0 MiB"
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
        assert_eq!(render_progress(&progress), "downloading 1/4, 2.0/8.0 KiB");
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
}
