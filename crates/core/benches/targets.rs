use std::path::PathBuf;

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::policy::Policy;
use mix_core::declared::targets::{UserConfig, tree};

fn main() {
    divan::main();
}

fn user_config() -> UserConfig {
    UserConfig {
        user: InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "mix-user".to_string(),
            home: PathBuf::from("/home/mix-user"),
        },
        flake: "f".repeat(512),
        lock: "lock-content".to_string(),
        home: "h".repeat(256),
        restored_state: None,
    }
}

#[divan::bench]
fn build_the_system_tree(bencher: divan::Bencher) {
    let policy = Policy::default();
    bencher.bench(|| tree(None, divan::black_box(&policy)));
}

#[divan::bench]
fn build_the_tree_with_a_user(bencher: divan::Bencher) {
    let cfg = user_config();
    let policy = Policy::default();
    bencher.bench(|| tree(Some(divan::black_box(&cfg)), divan::black_box(&policy)));
}

#[divan::bench]
fn label_and_categorize_every_check(bencher: divan::Bencher) {
    let cfg = user_config();
    let policy = Policy::default();
    let tree = tree(Some(&cfg), &policy);
    bencher.bench(|| {
        divan::black_box(&tree)
            .checks()
            .iter()
            .map(|check| (check.label(), check.target.category()))
            .collect::<Vec<_>>()
    });
}
