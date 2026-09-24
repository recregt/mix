use mix_core::state::StateManifest;

fn main() {
    divan::main();
}

fn manifest(n: usize) -> StateManifest {
    StateManifest {
        version: 1,
        packages: (0..n).map(|i| format!("package-{i}")).collect(),
    }
}

#[divan::bench(args = [1, 8, 64])]
fn render_the_state_manifest(bencher: divan::Bencher, n: usize) {
    let manifest = manifest(n);
    bencher.bench(|| divan::black_box(&manifest).render());
}

#[divan::bench(args = [1, 8, 64])]
fn parse_the_state_manifest(bencher: divan::Bencher, n: usize) {
    let raw = manifest(n).render();
    bencher.bench(|| StateManifest::parse(divan::black_box(&raw)).unwrap());
}

fn requested(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                format!("package-{i}")
            } else {
                format!("candidate-{i}")
            }
        })
        .collect()
}

#[divan::bench(args = [1, 8, 64])]
fn partition_the_requested_packages(bencher: divan::Bencher, n: usize) {
    let manifest = manifest(n);
    let requested = requested(n);
    bencher.bench(|| divan::black_box(&manifest).partition(divan::black_box(&requested)));
}

#[divan::bench(args = [1, 8, 64])]
fn drop_the_requested_packages(bencher: divan::Bencher, n: usize) {
    let manifest = manifest(n);
    let removed: Vec<String> = (0..n).step_by(2).map(|i| format!("package-{i}")).collect();
    bencher.bench(|| divan::black_box(&manifest).without(divan::black_box(&removed)));
}
