use indicatif::{ProgressBar, ProgressDrawTarget, TermLike};
use mix_core::BuildProgress;
use mix_ui::activity::{printable, write_progress};

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
fn clean_a_plain_line(bencher: divan::Bencher) {
    bencher.bench(|| printable(divan::black_box(PLAIN)));
}

#[divan::bench]
fn clean_a_coloured_line(bencher: divan::Bencher) {
    bencher.bench(|| printable(divan::black_box(COLOURED)));
}

#[divan::bench]
fn clean_a_long_line(bencher: divan::Bencher) {
    let line = long_line();
    bencher.bench(|| printable(divan::black_box(&line)));
}

#[divan::bench]
fn clean_a_wide_line(bencher: divan::Bencher) {
    let line = wide_line();
    bencher.bench(|| printable(divan::black_box(&line)));
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
/// Measured the way the frame is actually drawn, into the reporter's own buffer reused from one
/// frame to the next, so what is left is the digits and nothing else.
#[divan::bench]
fn render_the_build_counters(bencher: divan::Bencher) {
    let progress = counters();
    let mut frame = String::with_capacity(128);
    write_progress(&mut frame, &progress, "hello-2.12.3");

    bencher.bench_local(|| {
        frame.clear();
        write_progress(&mut frame, divan::black_box(&progress), "hello-2.12.3");
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

#[divan::bench]
fn draw_a_live_frame(bencher: divan::Bencher) {
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::term_like(Box::new(Discard)))
        .with_style(mix_ui::live_style())
        .with_prefix("Activating")
        .with_message("ripgrep");

    bencher.bench_local(|| bar.tick());
}
