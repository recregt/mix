use mix_ui::activity::{MAX_WIDTH, Throttle, display_line};

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
