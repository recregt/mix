use mix_ui::message::polished;
use mix_ui::{Status, status_line};

fn main() {
    divan::main();
}

/// A line `mix install` prints when it is done.
const PROSE: &str = "installed: ripgrep, fd, jq";

/// A line `mix doctor -v` prints once per check: a name that has to keep its own spelling.
const LABEL: &str = "/nix/var/nix/profiles/per-user";

/// A failure with the hint that goes under it, which is what most of the tool's errors look
/// like.
const FAILURE: &str = "the binary cache has nothing to download for: cowsay-3.8.4\n\
                       Installing this would compile it from source, which can take hours.\n\
                       To compile it anyway, re-run with `--build`:\n\
                       \x20 mix install --build ...";

/// What a printed line costs end to end: the decisions, the marker, and the one buffer they are
/// written into.
#[divan::bench]
fn build_a_status_line(bencher: divan::Bencher) {
    bencher.bench(|| status_line(Status::Done, divan::black_box(PROSE), false));
}

/// The same line on a terminal that takes colour, where the marker is drawn in green and the
/// backticked spans as code.
#[divan::bench]
fn build_a_coloured_status_line(bencher: divan::Bencher) {
    bencher.bench(|| status_line(Status::Failed, divan::black_box(FAILURE), true));
}

/// A name is the case the decisions exist for: nothing is capitalized, nothing is appended, and
/// the shape of the line is what says so.
#[divan::bench]
fn build_a_label_line(bencher: divan::Bencher) {
    bencher.bench(|| status_line(Status::Done, divan::black_box(LABEL), false));
}

#[divan::bench]
fn polish_a_sentence(bencher: divan::Bencher) {
    bencher.bench(|| polished(divan::black_box(PROSE)));
}

#[divan::bench]
fn polish_a_name(bencher: divan::Bencher) {
    bencher.bench(|| polished(divan::black_box(LABEL)));
}

#[divan::bench]
fn polish_a_failure_with_a_hint(bencher: divan::Bencher) {
    bencher.bench(|| polished(divan::black_box(FAILURE)));
}
