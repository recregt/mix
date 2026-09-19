use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle, TermLike};
use mix_core::BuildProgress;
use mix_ui::activity::{FrameBuffer, MAX_WIDTH, Throttle, display_line, write_progress};

fn main() {
    divan::main();
}

const PLAIN: &str = "copying path '/nix/store/000000000000000-glibc-2.40' from 'https://c'";
const COLOURED: &str =
    "\u{1b}[32mbuilding\u{1b}[0m '/nix/store/111111111111111-home-manager-generation.drv'";

fn long_line() -> String {
    format!(
        "copying path '/nix/store/{}-ripgrep-14.1.1'",
        "a".repeat(256)
    )
}

fn wide_line() -> String {
    "パッケージをダウンロードしています".repeat(8)
}

#[divan::bench]
fn render_a_plain_line(bencher: divan::Bencher) {
    bencher.bench(|| display_line(divan::black_box(PLAIN)));
}

#[divan::bench]
fn render_a_coloured_line(bencher: divan::Bencher) {
    bencher.bench(|| display_line(divan::black_box(COLOURED)));
}

#[divan::bench]
fn render_a_long_line(bencher: divan::Bencher) {
    let line = long_line();
    assert!(line.len() > MAX_WIDTH);
    bencher.bench(|| display_line(divan::black_box(&line)));
}

/// Truncating by column rather than by character is what keeps a line of wide characters from
/// wrapping the step's line; this is what that costs when it happens.
#[divan::bench]
fn render_a_wide_line(bencher: divan::Bencher) {
    let line = wide_line();
    assert!(line.len() > MAX_WIDTH);
    bencher.bench(|| display_line(divan::black_box(&line)));
}

/// What nix reports part way through installing a package.
fn counters() -> BuildProgress {
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

/// What a drawn frame costs once nix is reporting counters: a short, bounded string where only
/// the numbers change, instead of a line of free-form output that has to be scanned and trimmed.
///
/// Measured the way the frame is actually drawn — into the reporter's own buffer, reused from
/// one frame to the next — so what is left is the digits and nothing else.
#[divan::bench]
fn render_the_build_counters(bencher: divan::Bencher) {
    let progress = counters();
    let mut frame = String::with_capacity(128);
    write_progress(&mut frame, &progress);

    bencher.bench_local(|| {
        frame.clear();
        write_progress(&mut frame, divan::black_box(&progress));
        frame.len()
    });
}

/// A terminal that measures like a real one and throws away what is written to it, so what is
/// left is the cost of building the line rather than of the write.
#[derive(Debug)]
struct Discard;

impl TermLike for Discard {
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

    fn write_line(&self, _s: &str) -> std::io::Result<()> {
        Ok(())
    }

    fn write_str(&self, _s: &str) -> std::io::Result<()> {
        Ok(())
    }

    fn clear_line(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The keys the progress layer fills in for a step's span: its name, and the indent of a nested
/// one.
fn labelled(style: ProgressStyle, label: &'static str) -> ProgressStyle {
    style
        .with_key(
            "span_fields",
            move |_: &ProgressState, w: &mut dyn std::fmt::Write| {
                let _ = w.write_str(label);
            },
        )
        .with_key(
            "span_child_prefix",
            |_: &ProgressState, _: &mut dyn std::fmt::Write| {},
        )
}

/// What one turn of the spinner costs: the whole step line is rendered from its template and
/// trimmed to the terminal on every frame, whether or not anything but the spinner moved.
///
/// This is the work the frame rate is a budget for, and the reason for spending that budget on
/// frames the terminal will actually be shown.
#[divan::bench]
fn draw_a_spinner_frame(bencher: divan::Bencher) {
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::term_like(Box::new(Discard)))
        .with_style(labelled(mix_ui::step_style(), "Installing ripgrep"));
    bar.set_message("\u{1b}[2mbuilding  3/17 · downloading 12/37 · 48.2/91.0 MiB\u{1b}[0m");

    bencher.bench_local(|| bar.tick());
}

/// The frame a flooding build most often produces is the one already on screen: a counter it
/// reports twice, or a line that trims to what the last one trimmed to. Dropping it here costs a
/// comparison, where drawing it would cost [`draw_a_spinner_frame`] plus the handover.
#[divan::bench]
fn skip_an_unchanged_frame(bencher: divan::Bencher) {
    let progress = counters();
    let mut frames = FrameBuffer::new();
    frames.build(|frame| write_progress(frame, &progress));

    bencher.bench_local(|| {
        frames
            .build(|frame| write_progress(frame, divan::black_box(&progress)))
            .is_some()
    });
}

/// The cost a flooding process actually pays: the frame is not due, so the line is dropped.
/// The interval is pinned well past the benchmark's runtime so every call takes that path.
#[divan::bench(args = [64, 4096])]
fn drop_a_burst_of_lines(bencher: divan::Bencher, lines: usize) {
    let throttle = Throttle::with_interval_ms(60 * 60 * 1000);
    throttle.due();

    bencher.bench(|| {
        let mut drawn = 0usize;
        for _ in 0..divan::black_box(lines) {
            if throttle.due() {
                drawn += 1;
            }
        }
        drawn
    });
}
