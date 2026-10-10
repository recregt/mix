#![allow(clippy::disallowed_methods)]

use std::path::Path;
use std::sync::Arc;

use mix_core::declared::targets::Target;
use mix_core::ops::health;
use mix_core::run::Runner;
use mix_core::run::journal::Record;
use mix_events::v1::Command;
use mix_events::{Ending, Outbox, ROOT, Start, Tree};
use mix_shell::drive::{Performer, drive};
use mix_shell::effect::files::Files;

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
fn declared_tree(root: &std::path::Path, n: usize) -> Vec<Target<'static>> {
    let mut items = Vec::with_capacity(n * 4);
    for i in 0..n {
        let dir = root.join(format!("store/{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nix.conf"), "declared contents\n").unwrap();
        std::fs::write(dir.join("flake.lock"), "whatever nix wrote\n").unwrap();
        std::fs::write(dir.join("state"), "{\"version\":1,\"packages\":[]}").unwrap();

        items.push(Target::Directory {
            path: dir.clone().into(),
            mode: mode_of(&dir),
            owner: None,
        });
        items.push(Target::File {
            path: dir.join("nix.conf").into(),
            expected: Some("declared contents\n".to_string().into()),
            owner: None,
        });
        items.push(Target::File {
            path: dir.join("flake.lock").into(),
            expected: None,
            owner: None,
        });
        items.push(Target::SeededFile {
            path: dir.join("state").into(),
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

fn performer() -> Performer {
    Performer::new(Files::open(Path::new("/"), "bench").unwrap())
}

fn repair(
    rt: &tokio::runtime::Runtime,
    performer: &mut Performer,
    targets: Vec<Target<'static>>,
) -> Runner {
    let outbox = Arc::new(Outbox::new("bench", || {}));
    let mut tree = Tree::new(
        Arc::clone(&outbox),
        Arc::new(|| None),
        Start::command("repair", Command::default()),
    );
    let mut runner = Runner::new(ROOT, health::target_steps(targets, "bench")).independent();
    let mut journal: Vec<Record> = Vec::new();
    let scope = mix_exec::Scope::root();
    let stopped: mix_events::Stopped = Arc::new(|| None);
    let report = rt.block_on(drive(
        &mut runner,
        &mut tree,
        performer,
        &scope,
        &stopped,
        &mut journal,
        &mut (),
    ));
    let _ = tree.finish(ROOT, Ending::succeeded());
    drop(tree);
    divan::black_box(outbox.drain());
    divan::black_box(report);
    runner
}

/// The measurement both `mix doctor` and `mix repair` read: every declared target inspected once.
#[divan::bench(args = [1, 8, 64])]
fn inspect_the_declared_targets(bencher: divan::Bencher, n: usize) {
    let root = tempfile::tempdir().unwrap();
    let targets = declared_tree(root.path(), n);
    let rt = runtime();
    let mut performer = performer();
    let scope = mix_exec::Scope::root();

    bencher.bench_local(|| {
        rt.block_on(async {
            let mut drifted = 0usize;
            for item in divan::black_box(&targets) {
                let facts = performer
                    .observe(&health::queries(item), &scope)
                    .await
                    .unwrap();
                drifted += usize::from(health::classify(item, &facts).is_some());
            }
            drifted
        })
    });
}

/// A whole `mix repair` run on a system that has not drifted: every target observed, classified and
/// left alone, with the plan, journal and events around it.
#[divan::bench(args = [1, 8, 64])]
fn repair_the_declared_targets(bencher: divan::Bencher, n: usize) {
    let root = tempfile::tempdir().unwrap();
    let targets = declared_tree(root.path(), n);
    let rt = runtime();
    let mut performer = performer();

    bencher
        .with_inputs(|| targets.clone())
        .bench_local_values(|targets| repair(&rt, &mut performer, targets));
}

/// A whole `mix repair` run over one file whose contents drifted: observed, classified, replaced
/// atomically under its precondition and committed.
#[divan::bench]
fn repair_a_file_that_drifted(bencher: divan::Bencher) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("nix.conf");
    let target = Target::File {
        path: path.clone().into(),
        expected: Some("build-users-group = nixbld\n".to_string().into()),
        owner: None,
    };
    let rt = runtime();
    let mut performer = performer();

    bencher
        .with_inputs(|| {
            std::fs::write(&path, "drifted\n").unwrap();
            vec![target.clone()]
        })
        .bench_local_values(|targets| repair(&rt, &mut performer, targets));
}
