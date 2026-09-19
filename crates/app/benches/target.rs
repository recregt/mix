use mix_app::target::{self, Finding};
use mix_core::CancellationToken;
use mix_core::models::Target;

fn main() {
    divan::main();
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the benchmark")
}

/// A tree shaped like the one mix declares: directories with a mode, a file it writes, a file it
/// only owns, and a seeded file it never rewrites.
fn declared_tree(root: &std::path::Path, n: usize) -> Vec<Target> {
    let mut items = Vec::with_capacity(n * 4);
    for i in 0..n {
        let dir = root.join(format!("store/{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nix.conf"), "declared contents\n").unwrap();
        std::fs::write(dir.join("flake.lock"), "whatever nix wrote\n").unwrap();
        std::fs::write(dir.join("state"), "{\"version\":1,\"packages\":[]}").unwrap();

        items.push(Target::Directory {
            path: dir.clone(),
            mode: mode_of(&dir),
            owner: None,
        });
        items.push(Target::File {
            path: dir.join("nix.conf"),
            expected: Some("declared contents\n".to_string()),
            owner: None,
        });
        items.push(Target::File {
            path: dir.join("flake.lock"),
            expected: None,
            owner: None,
        });
        items.push(Target::SeededFile {
            path: dir.join("state"),
            seed: "{\"version\":1,\"packages\":[]}".into(),
            owner: None,
        });
    }
    items
}

fn mode_of(path: &std::path::Path) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path).unwrap().permissions())
        & 0o7777
}

/// The measurement both `mix doctor` and `mix repair` read: every declared target inspected once.
#[divan::bench(args = [1, 8, 64])]
fn inspect_the_declared_targets(bencher: divan::Bencher, n: usize) {
    let root = tempfile::tempdir().unwrap();
    let targets = declared_tree(root.path(), n);
    let rt = runtime();

    bencher.bench_local(|| {
        rt.block_on(async {
            let mut drifted = 0usize;
            for item in divan::black_box(&targets) {
                drifted += usize::from(target::inspect(item).await.is_some());
            }
            drifted
        })
    });
}

/// What `mix repair` costs on a system that has not drifted: the targets are measured, and
/// nothing is measured a second time to find that out.
#[divan::bench(args = [1, 8, 64])]
fn apply_the_declared_targets(bencher: divan::Bencher, n: usize) {
    let root = tempfile::tempdir().unwrap();
    let targets = declared_tree(root.path(), n);
    let token = CancellationToken::new();
    let rt = runtime();

    bencher.bench_local(|| {
        rt.block_on(async {
            let mut repaired = 0usize;
            for item in divan::black_box(&targets) {
                repaired += usize::from(target::apply(item, &token).await.unwrap());
            }
            repaired
        })
    });
}

/// A file mix writes whose contents drifted: what the reconciliation costs once the measurement
/// has already been taken.
#[divan::bench]
fn reconcile_a_file_that_drifted(bencher: divan::Bencher) {
    let root = tempfile::tempdir().unwrap();
    let target = Target::File {
        path: root.path().join("nix.conf"),
        expected: Some("build-users-group = nixbld\n".to_string()),
        owner: None,
    };
    let token = CancellationToken::new();
    let rt = runtime();

    bencher.bench_local(|| {
        rt.block_on(target::reconcile(
            divan::black_box(&target),
            Finding::ContentDrift,
            &token,
        ))
        .unwrap()
    });
}

/// An owner that drifted: the inspection measured both sides of it, so the reconciliation sets
/// the owner without reading the artifact again.
#[divan::bench]
fn reconcile_an_owner_that_drifted(bencher: divan::Bencher) {
    let root = tempfile::tempdir().unwrap();
    let owner = (
        nix::unistd::Uid::current().as_raw(),
        nix::unistd::Gid::current().as_raw(),
    );
    let target = Target::Directory {
        path: root.path().to_path_buf(),
        mode: mode_of(root.path()),
        owner: Some(owner),
    };
    let finding = Finding::Owner {
        actual: (0, 0),
        expected: owner,
    };
    let token = CancellationToken::new();
    let rt = runtime();

    bencher.bench_local(|| {
        rt.block_on(target::reconcile(
            divan::black_box(&target),
            finding,
            &token,
        ))
        .unwrap()
    });
}
