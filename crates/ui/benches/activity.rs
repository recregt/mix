use mix_core::BuildProgress;
use mix_ui::activity::{MAX_WIDTH, Throttle, display_line, render_progress};

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

/// What a drawn frame costs once nix is reporting counters: a short, bounded string where only
/// the numbers change, instead of a line of free-form output that has to be scanned and trimmed.
#[divan::bench]
fn render_the_build_counters(bencher: divan::Bencher) {
    let progress = BuildProgress {
        builds_done: 3,
        builds_expected: 17,
        builds_running: 1,
        downloads_done: 12,
        downloads_expected: 37,
        downloads_running: 2,
        bytes_done: 50_525_798,
        bytes_expected: 95_420_416,
    };

    bencher.bench(|| render_progress(divan::black_box(&progress)));
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
