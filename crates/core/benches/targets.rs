use std::path::PathBuf;

use mix_core::models::{UserConfig, targets, user_targets};
use mix_core::privilege::InvokingUser;

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
        home: "h".repeat(256),
    }
}

fn trusted_users() -> Vec<String> {
    vec!["mix-user".to_string(), "other-user".to_string()]
}

#[divan::bench]
fn build_the_system_target_list(bencher: divan::Bencher) {
    bencher.bench(|| targets(None, &[]));
}

#[divan::bench]
fn build_the_target_list_with_a_user(bencher: divan::Bencher) {
    let cfg = user_config();
    let trusted = trusted_users();
    bencher.bench(|| targets(Some(divan::black_box(&cfg)), divan::black_box(&trusted)));
}

#[divan::bench]
fn build_the_per_user_target_list(bencher: divan::Bencher) {
    let cfg = user_config();
    bencher.bench(|| user_targets(divan::black_box(&cfg)));
}

#[divan::bench]
fn label_and_categorize_every_target(bencher: divan::Bencher) {
    let cfg = user_config();
    let items = targets(Some(&cfg), &trusted_users());
    bencher.bench(|| {
        divan::black_box(&items)
            .iter()
            .map(|target| (target.label(), target.category()))
            .collect::<Vec<_>>()
    });
}
