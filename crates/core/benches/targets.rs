use std::path::PathBuf;

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::policy::Policy;
use mix_core::declared::targets::{UserConfig, targets, user_targets};

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
fn build_the_system_target_list(bencher: divan::Bencher) {
    let policy = Policy::default();
    bencher.bench(|| targets(None, divan::black_box(&policy)));
}

#[divan::bench]
fn build_the_target_list_with_a_user(bencher: divan::Bencher) {
    let cfg = user_config();
    let policy = Policy::default();
    bencher.bench(|| targets(Some(divan::black_box(&cfg)), divan::black_box(&policy)));
}

#[divan::bench]
fn build_the_per_user_target_list(bencher: divan::Bencher) {
    let cfg = user_config();
    bencher.bench(|| user_targets(divan::black_box(&cfg)));
}

#[divan::bench]
fn label_and_categorize_every_target(bencher: divan::Bencher) {
    let cfg = user_config();
    let policy = Policy::default();
    let items = targets(Some(&cfg), &policy);
    bencher.bench(|| {
        divan::black_box(&items)
            .iter()
            .map(|target| (target.label(), target.category()))
            .collect::<Vec<_>>()
    });
}
