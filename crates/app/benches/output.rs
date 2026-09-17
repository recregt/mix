use mix_app::output::{LineSplitter, StreamDrain, TailBuffer};
use mix_core::{ActivityReporter, NoopActivity};

fn main() {
    divan::main();
}

const KIB: usize = 1024;

/// A chunk shaped like what `nix build` writes to stderr: short status lines, a few of them
/// redrawn in place with a carriage return, and the occasional long store path.
fn nix_output(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..lines {
        match n % 4 {
            0 => out.extend_from_slice(b"copying path '/nix/store/0000000000000000000000000000000-glibc-2.40-36' from 'https://cache.nixos.org'\n"),
            1 => out.extend_from_slice(b"building '/nix/store/1111111111111111111111111111111-home-manager-generation.drv'\n"),
            2 => out.extend_from_slice(b"[2/17 built] fetching ripgrep\r"),
            _ => out.extend_from_slice(b"unpacking source\n"),
        }
    }
    out
}

/// Chunks the way the reader sees them: fixed-size reads that land mid-line.
fn chunks(bytes: &[u8], size: usize) -> Vec<&[u8]> {
    bytes.chunks(size).collect()
}

#[divan::bench(args = [64, 512, 4096])]
fn split_streamed_output_into_lines(bencher: divan::Bencher, lines: usize) {
    let bytes = nix_output(lines);
    let chunks = chunks(&bytes, 8 * KIB);

    bencher.bench_local(|| {
        let mut splitter = LineSplitter::new();
        let mut seen = 0usize;
        for chunk in &chunks {
            splitter.push(divan::black_box(chunk), |line| seen += line.len());
        }
        splitter.finish(|line| seen += line.len());
        seen
    });
}

/// A stream shaped like what `nix build --log-format internal-json` writes: mostly progress
/// records, an activity starting or stopping now and then, and the occasional diagnostic.
fn nix_records(records: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..records {
        match n % 8 {
            0 => out.extend_from_slice(
                br#"@nix {"action":"start","id":7,"level":3,"text":"","type":100,"fields":["/nix/store/0000000000000000000000000000000-glibc-2.40-36","https://cache.nixos.org"]}"#,
            ),
            7 => out.extend_from_slice(br#"@nix {"action":"stop","id":7}"#),
            5 => out.extend_from_slice(
                br#"@nix {"action":"msg","level":3,"msg":"copying path '/nix/store/1111111111111111111111111111111-ripgrep-14.1.1'"}"#,
            ),
            _ => out.extend_from_slice(
                br#"@nix {"action":"result","id":7,"type":105,"fields":[50525798,95420416,0,0]}"#,
            ),
        }
        out.push(b'\n');
    }
    out
}

/// What a build costs to follow: every chunk nix writes is split into lines, folded into the
/// counters, and reported — for a stream whose raw bytes are never shown to anyone, so keeping
/// them is work that ends in the bin.
#[divan::bench(args = [64, 512, 4096])]
fn drain_a_structured_stream(bencher: divan::Bencher, records: usize) {
    let bytes = nix_records(records);
    let chunks = chunks(&bytes, 8 * KIB);
    let activity: &dyn ActivityReporter = &NoopActivity;

    bencher.bench_local(|| {
        let mut drain = StreamDrain::new(64 * KIB);
        for chunk in &chunks {
            drain.push(divan::black_box(chunk), activity);
        }
        drain.finish(activity).len()
    });
}

/// The same path for a process that writes prose — an activation script, a `useradd` — where
/// the raw bytes are the only thing a failure could be explained with, so they are kept.
#[divan::bench(args = [64, 512, 4096])]
fn drain_a_plain_stream(bencher: divan::Bencher, lines: usize) {
    let bytes = nix_output(lines);
    let chunks = chunks(&bytes, 8 * KIB);
    let activity: &dyn ActivityReporter = &NoopActivity;

    bencher.bench_local(|| {
        let mut drain = StreamDrain::new(64 * KIB);
        for chunk in &chunks {
            drain.push(divan::black_box(chunk), activity);
        }
        drain.finish(activity).len()
    });
}

#[divan::bench(args = [64, 4096])]
fn retain_the_tail_of_streamed_output(bencher: divan::Bencher, lines: usize) {
    let bytes = nix_output(lines);
    let chunks = chunks(&bytes, 8 * KIB);

    bencher.bench_local(|| {
        let mut tail = TailBuffer::new(64 * KIB);
        for chunk in &chunks {
            tail.extend(divan::black_box(chunk));
        }
        tail.into_bytes().len()
    });
}
