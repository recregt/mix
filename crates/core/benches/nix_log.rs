use mix_core::nix_log::{Event, NixLog};

fn main() {
    divan::main();
}

/// A stream shaped like the one `nix build --log-format internal-json` writes while it fetches a
/// closure: one activity per path, progress reported as the bytes arrive, and the aggregate
/// counters refreshed in between.
fn records(paths: usize) -> Vec<String> {
    let mut lines = vec![
        r#"@nix {"action":"start","id":1,"level":3,"parent":0,"text":"","type":103,"fields":[]}"#
            .to_string(),
        r#"@nix {"action":"start","id":2,"level":3,"parent":0,"text":"","type":104,"fields":[]}"#
            .to_string(),
        r#"@nix {"action":"result","id":1,"type":106,"fields":[100,94371840]}"#.to_string(),
    ];

    for path in 0..paths {
        let id = 100 + path as u64;
        lines.push(format!(
            r#"@nix {{"action":"start","id":{id},"level":3,"parent":1,"text":"copying path '/nix/store/{path:032}-package-{path}' from 'https://cache.nixos.org'","type":100,"fields":["/nix/store/{path:032}-package-{path}","https://cache.nixos.org","local"]}}"#
        ));
        for step in 1..=4u64 {
            lines.push(format!(
                r#"@nix {{"action":"result","id":{id},"type":105,"fields":[{},262144,0,0]}}"#,
                step * 65_536
            ));
        }
        lines.push(format!(r#"@nix {{"action":"stop","id":{id}}}"#));
        lines.push(format!(
            r#"@nix {{"action":"result","id":1,"type":105,"fields":[{},{paths},1,0]}}"#,
            path + 1
        ));
    }

    lines.push(r#"@nix {"action":"stop","id":2}"#.to_string());
    lines.push(r#"@nix {"action":"stop","id":1}"#.to_string());
    lines
}

/// The cost of the whole pipeline: parse every record, fold it into the counters, and read the
/// snapshot the screen would be drawn from.
#[divan::bench(args = [64, 512, 4096])]
fn track_a_nix_build(bencher: divan::Bencher, paths: usize) {
    let records = records(paths / 6);

    bencher.bench_local(|| {
        let mut log = NixLog::new();
        let mut snapshots = 0usize;
        for record in &records {
            if let Event::Progress = log.observe(divan::black_box(record)) {
                snapshots += log.snapshot().bytes_done as usize;
            }
        }
        snapshots
    });
}

/// The single hottest record: a byte count for a path that is still being copied.
#[divan::bench]
fn fold_a_progress_record(bencher: divan::Bencher) {
    let mut log = NixLog::new();
    log.observe(r#"@nix {"action":"start","id":7,"level":3,"text":"","type":100,"fields":[]}"#);
    let record = r#"@nix {"action":"result","id":7,"type":105,"fields":[262144,1048576,0,0]}"#;

    bencher.bench_local(|| log.observe(divan::black_box(record)));
}

/// A line that is not a record at all: what the activation script's output costs to look at.
#[divan::bench]
fn pass_through_a_plain_line(bencher: divan::Bencher) {
    let mut log = NixLog::new();
    let line = "Activating home-manager generation 42";

    bencher.bench_local(|| log.observe(divan::black_box(line)));
}
