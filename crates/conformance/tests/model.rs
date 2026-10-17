use std::path::PathBuf;
use std::sync::Arc;

use mix_conformance::model::{Model, alice};
use mix_conformance::suite::{
    Command, FaultKind, Mix, Spec, Transition, bootstrap, doctor, findings, healthy, install,
    repair, succeeded, violations,
};
use mix_core::model::World;
use mix_core::model::testkit::{Breakage, Damage, breakages, owned};
use proptest::prelude::ProptestConfig;
use proptest_state_machine::{StateMachineTest, prop_state_machine};

#[tokio::test]
async fn the_whole_request_path_runs_on_the_model() {
    let model = Model::new();

    let bootstrapped = model.run(bootstrap(), false, true).await;
    assert!(succeeded(&bootstrapped), "{bootstrapped:#}");

    let before = model.snapshot();
    let predicted = model.run(install(&["hello"]), true, false).await;
    assert!(succeeded(&predicted), "{predicted:#}");
    assert!(model.snapshot() == before, "a dry run changed the model");

    let installed = model.run(install(&["hello"]), false, false).await;
    assert!(succeeded(&installed), "{installed:#}");
    assert_eq!(
        predicted["changes"].as_array().map(Vec::len),
        installed["changes"].as_array().map(Vec::len)
    );
    assert_eq!(installed["install"]["added"], serde_json::json!(["hello"]));

    let checked = model.run(doctor(), false, false).await;
    assert!(succeeded(&checked), "{checked:#}");
    let listed = model
        .snapshot()
        .contents("/home/alice/.local/state/mix/state")
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    assert!(listed.contains("\"hello\""), "{listed}");
    assert!(findings(&checked).is_empty(), "{checked:#}");

    let before = model.snapshot();
    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    assert!(
        model.snapshot() == before,
        "repair changed a healthy machine: {repaired:#}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn any_single_damage_to_what_mix_owns_is_repaired_as_planned() {
    let model = Model::new();
    let pristine = model.snapshot();
    model.run(bootstrap(), false, true).await;
    model.run(install(&["hello"]), false, false).await;
    let cases = breakages(&owned(&pristine, &model.snapshot()));
    assert!(!cases.is_empty());

    let mut checks = tokio::task::JoinSet::new();
    for breakage in cases {
        let fork = model.fork();
        checks.spawn(async move { violations(&fork, &breakage).await });
    }
    let mut found = Vec::new();
    while let Some(violations) = checks.join_next().await {
        found.extend(violations.unwrap());
    }
    found.sort();

    assert!(
        found.is_empty(),
        "{} violations:\n{}",
        found.len(),
        found.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_history_that_cannot_be_created_puts_the_old_repository_back() {
    let model = Model::new();
    model.run(bootstrap(), false, true).await;
    model.run(install(&["hello"]), false, false).await;
    let repository = mix_core::declared::paths::repository_dir(&model.user().home);
    Breakage {
        path: repository.join(mix_core::declared::paths::REPOSITORY_HEAD),
        damage: Damage::Altered,
    }
    .apply(&mut model.world.lock().unwrap_or_else(|e| e.into_inner()));
    let history = |model: &Model| -> Vec<_> {
        let world = model.snapshot();
        world
            .files
            .into_iter()
            .filter(|(path, _)| path.starts_with(&repository))
            .map(|(path, entry)| (path, entry.content))
            .collect()
    };
    let damaged = history(&model);

    for at in 0.. {
        let fork = model.fork();
        let Some(done) = fork.faulted(repair(), true, at, FaultKind::Fail).await else {
            continue;
        };
        let failed: Vec<&serde_json::Value> = done["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|change| change["status"] == "STATUS_FAILED")
            .collect();
        assert!(
            !failed.is_empty(),
            "no step created the repository: {done:#}"
        );
        if failed
            .iter()
            .any(|change| change["operation"] == "OPERATION_CREATE_REPOSITORY")
        {
            assert_eq!(history(&fork), damaged, "{done:#}");
            return;
        }
    }
}

prop_state_machine! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/model.proptest-regressions"
            ))
        )),
        ..proptest::prelude::ProptestConfig::default()
    })]
    #[test]
    fn any_sequence_of_commands_damage_and_faults_keeps_every_invariant(
        sequential 1..8 => Mix
    );
}

fn object_holding(file: &str) -> PathBuf {
    let user = alice();
    let staged = healthy().world.staged(&user);
    let bytes = staged
        .get(file)
        .unwrap_or_else(|| panic!("mix records no {file} in its repository"));
    World::blob_object(&user, bytes)
}

fn replay(transitions: Vec<Transition>) {
    let owned = &healthy().owned;
    for transition in &transitions {
        if let Transition::Damage(breakage) = transition {
            assert!(
                owned.contains(&breakage.path),
                "{} is no longer something mix owns, so this case would damage nothing",
                breakage.path.display()
            );
        }
    }
    Mix::test_sequential(
        ProptestConfig::default(),
        Spec {
            owned: Arc::clone(owned),
            ended: false,
        },
        transitions,
        None,
    );
}

#[test]
fn a_failed_remove_over_a_damaged_repository_is_rolled_back() {
    replay(vec![
        Transition::Faulted(Command::Install("hello"), 4, FaultKind::Fail),
        Transition::Damage(Breakage {
            path: object_holding("flake.nix"),
            damage: Damage::Removed,
        }),
        Transition::Faulted(Command::Remove("hello"), 5, FaultKind::Fail),
    ]);
}

#[test]
fn an_install_over_an_unreadable_git_object_and_a_missing_gitignore() {
    replay(vec![
        Transition::Damage(Breakage {
            path: object_holding(".gitignore"),
            damage: Damage::Unreadable,
        }),
        Transition::Damage(Breakage {
            path: "/home/alice/.local/state/mix/.gitignore".into(),
            damage: Damage::Removed,
        }),
        Transition::Run(Command::Install("hello")),
    ]);
}

#[test]
fn a_repair_after_an_install_crashed_over_a_swapped_git_object() {
    replay(vec![
        Transition::Damage(Breakage {
            path: object_holding("home.nix"),
            damage: Damage::Swapped,
        }),
        Transition::Faulted(Command::Install("hello"), 3, FaultKind::CrashAfter),
        Transition::Run(Command::Repair),
    ]);
}

#[test]
fn an_install_after_one_crashed_and_one_failed() {
    replay(vec![
        Transition::Faulted(Command::Install("hello"), 4, FaultKind::CrashBefore),
        Transition::Faulted(Command::Install("hello"), 0, FaultKind::Fail),
        Transition::Run(Command::Install("hello")),
    ]);
}

#[test]
fn a_swapped_state_directory_after_an_install_crashed_before_its_first_change() {
    replay(vec![
        Transition::Faulted(Command::Install("hello"), 1, FaultKind::CrashBefore),
        Transition::Damage(Breakage {
            path: "/home/alice/.local".into(),
            damage: Damage::Swapped,
        }),
    ]);
}

#[test]
fn a_swapped_git_object_after_a_clean_that_followed_a_crashed_install() {
    replay(vec![
        Transition::Faulted(Command::Install("hello"), 3, FaultKind::CrashBefore),
        Transition::Run(Command::Clean(false)),
        Transition::Damage(Breakage {
            path: object_holding("home.nix"),
            damage: Damage::Swapped,
        }),
    ]);
}

#[test]
fn a_remove_with_the_flake_lock_missing() {
    replay(vec![
        Transition::Damage(Breakage {
            path: "/home/alice/.local/state/mix/flake.lock".into(),
            damage: Damage::Removed,
        }),
        Transition::Run(Command::Remove("hello")),
    ]);
}

#[test]
fn updates_on_either_side_of_a_removed_state_directory() {
    replay(vec![
        Transition::Run(Command::Update),
        Transition::Damage(Breakage {
            path: "/home/alice/.local".into(),
            damage: Damage::Removed,
        }),
        Transition::Run(Command::Update),
    ]);
}

#[test]
fn a_list_after_an_install_crashed_before_its_first_change() {
    replay(vec![
        Transition::Faulted(Command::Install("hello"), 0, FaultKind::CrashBefore),
        Transition::Run(Command::List),
    ]);
}

#[test]
fn an_update_after_a_failed_install_and_a_crashed_remove() {
    replay(vec![
        Transition::Faulted(Command::Install("ripgrep"), 5, FaultKind::Fail),
        Transition::Faulted(Command::Remove("ripgrep"), 3, FaultKind::CrashBefore),
        Transition::Run(Command::Update),
    ]);
}

#[test]
fn an_unreadable_git_object_after_an_install_with_the_flake_lock_missing() {
    replay(vec![
        Transition::Damage(Breakage {
            path: "/home/alice/.local/state/mix/flake.lock".into(),
            damage: Damage::Removed,
        }),
        Transition::Run(Command::Install("hello")),
        Transition::Damage(Breakage {
            path: object_holding("flake.lock"),
            damage: Damage::Unreadable,
        }),
    ]);
}

#[test]
fn a_remove_and_a_repair_over_a_swapped_git_object() {
    replay(vec![
        Transition::Run(Command::Install("hello")),
        Transition::Damage(Breakage {
            path: object_holding("home.nix"),
            damage: Damage::Swapped,
        }),
        Transition::Run(Command::Remove("hello")),
        Transition::Run(Command::Repair),
    ]);
}

#[test]
fn an_update_after_a_crashed_clean_over_a_swapped_package_list() {
    replay(vec![
        Transition::Damage(Breakage {
            path: "/home/alice/.local/state/mix/state".into(),
            damage: Damage::Swapped,
        }),
        Transition::Faulted(Command::Clean(true), 0, FaultKind::CrashAfter),
        Transition::Run(Command::Update),
    ]);
}

#[test]
fn a_repair_after_an_update_crashed_and_the_local_directory_was_swapped() {
    replay(vec![
        Transition::Faulted(Command::Update, 1, FaultKind::CrashBefore),
        Transition::Damage(Breakage {
            path: "/home/alice/.local".into(),
            damage: Damage::Swapped,
        }),
        Transition::Run(Command::Repair),
    ]);
}

#[test]
fn a_repair_over_missing_heads_in_an_unreadable_refs_directory() {
    replay(vec![
        Transition::Damage(Breakage {
            path: "/home/alice/.local/state/mix/.git/refs/heads".into(),
            damage: Damage::Removed,
        }),
        Transition::Damage(Breakage {
            path: "/home/alice/.local/state/mix/.git/refs".into(),
            damage: Damage::Unreadable,
        }),
    ]);
}

#[test]
fn a_repair_over_a_swapped_commit_and_missing_heads() {
    let commit = healthy()
        .world
        .branch_object(&alice())
        .expect("mix records a commit");
    replay(vec![
        Transition::Damage(Breakage {
            path: commit,
            damage: Damage::Swapped,
        }),
        Transition::Damage(Breakage {
            path: "/home/alice/.local/state/mix/.git/refs/heads".into(),
            damage: Damage::Removed,
        }),
        Transition::Run(Command::Repair),
    ]);
}

#[test]
fn unreadable_objects_after_a_failed_repair_of_a_removed_local_directory() {
    replay(vec![
        Transition::Damage(Breakage {
            path: "/home/alice/.local".into(),
            damage: Damage::Removed,
        }),
        Transition::Faulted(Command::Repair, 4, FaultKind::Fail),
        Transition::Damage(Breakage {
            path: "/home/alice/.local/state/mix/.git/objects".into(),
            damage: Damage::Unreadable,
        }),
    ]);
}

#[test]
fn a_failed_purge_over_an_emptied_unit_is_rolled_back() {
    replay(vec![
        Transition::Damage(Breakage {
            path: "/etc/systemd/system/mix-daemon.service".into(),
            damage: Damage::Emptied,
        }),
        Transition::Faulted(Command::Purge, 5, FaultKind::Fail),
    ]);
}

#[test]
fn a_purge_after_a_purge_it_cannot_roll_back_leaves_no_journal() {
    replay(vec![
        Transition::Damage(Breakage {
            path: "/etc/systemd/system/mix-daemon.socket".into(),
            damage: Damage::Removed,
        }),
        Transition::Faulted(Command::Purge, 4, FaultKind::CrashBefore),
    ]);
}
