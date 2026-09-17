use mix_app::output::{LineSplitter, TailBuffer};

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
