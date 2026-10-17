use std::path::Path;

use mix_events::ROOT;
use mix_events::v1::UserRemoveRequest;
use mix_events::v1::command::Request;

use super::*;
use crate::declared::paths::{MIX_STATE_DIR, NIX_STORE, mix_state_dir};
use crate::model::World;
use crate::model::testkit::{
    Run, Script, account, difference, drive, enrolled, recover, requested,
};
use crate::run::{Runner, Verdict};

fn alice() -> InvokingUser {
    account("alice", 1000)
}

fn bob() -> InvokingUser {
    account("bob", 1001)
}

fn machine_of(users: &[InvokingUser]) -> World {
    let mut world = enrolled(users);
    for user in users {
        world.with_file(
            format!("{}/notes", user.home.display()),
            b"mine",
            0o644,
            (user.uid, user.gid),
        );
    }
    world
}

fn remove(world: &mut World, user: &InvokingUser, script: &Script) -> Run {
    drive(
        world,
        Runner::new(ROOT, steps(&Settings { user: user.clone() })),
        requested(Request::UserRemove(UserRemoveRequest {})),
        script,
    )
}

fn present(world: &World, path: &str) -> bool {
    world.files.contains_key(Path::new(path))
}

fn members(world: &World) -> Vec<String> {
    world.groups[MIX_USERS_GROUP].members.clone()
}

#[test]
fn a_user_removal_takes_one_user_and_leaves_the_machine_and_everyone_else() {
    let mut world = machine_of(&[alice(), bob()]);
    assert_eq!(members(&world), ["alice", "bob"]);

    let run = remove(&mut world, &alice(), &Script::default());

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert!(world.pending().is_empty(), "{:?}", world.pending());
    assert!(!present(
        &world,
        &mix_state_dir(&alice().home).to_string_lossy()
    ));
    assert!(present(&world, "/home/alice/notes"));
    assert_eq!(members(&world), ["bob"]);
    assert!(present(
        &world,
        &mix_state_dir(&bob().home).to_string_lossy()
    ));
    for kept in [NIX_STORE, "/etc/mix", "/etc/nix/nix.conf"] {
        assert!(present(&world, kept), "{kept} was removed with one user");
    }
    assert!(world.groups.contains_key(MIX_USERS_GROUP));
}

#[test]
fn removing_the_last_user_still_leaves_the_machine_to_purge() {
    let mut world = machine_of(&[alice()]);

    let run = remove(&mut world, &alice(), &Script::default());

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert!(members(&world).is_empty());
    assert!(present(&world, NIX_STORE));
    assert!(!present(&world, &format!("/home/alice/{MIX_STATE_DIR}")));
}

#[test]
fn a_user_who_never_enrolled_has_nothing_to_remove() {
    let mut world = machine_of(&[alice()]);
    let before = world.clone();

    let run = remove(&mut world, &bob(), &Script::default());

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert_eq!(difference(&before, &world), serde_json::json!({}));
}

#[test]
fn a_failure_before_the_commit_puts_the_user_back() {
    let mut world = machine_of(&[alice(), bob()]);
    let before = world.clone();
    let membership = |action: &Action| matches!(action, Action::RemoveMember { .. });

    let run = remove(
        &mut world,
        &alice(),
        &Script {
            fail_when: Some(membership),
            ..Script::default()
        },
    );

    assert!(matches!(run.report().verdict, Verdict::Failed { .. }));
    assert_eq!(difference(&before, &world), serde_json::json!({}));
}

#[test]
fn a_crash_at_any_action_ends_as_either_the_old_state_or_a_finished_removal() {
    let before = machine_of(&[alice(), bob()]);
    let mut finished = before.clone();
    remove(&mut finished, &alice(), &Script::default());
    let mut crash_at = 0;
    loop {
        let mut tried = false;
        for after in [false, true] {
            let mut world = before.clone();
            let crashed = remove(&mut world, &alice(), &Script::crashing(crash_at, after));
            if !crashed.crashed {
                continue;
            }
            tried = true;

            recover(&mut world, &crashed.journal);

            let restored = difference(&before, &world) == serde_json::json!({});
            let removed = difference(&finished, &world) == serde_json::json!({});
            assert!(
                restored || removed,
                "crash at action {crash_at}, after it: {after}, left:\n{:#}",
                difference(&before, &world)
            );
        }
        if !tried {
            break;
        }
        crash_at += 1;
    }
    assert!(crash_at > 3, "too few actions to crash in: {crash_at}");
}

#[test]
fn a_user_removal_discards_no_tree_so_it_shows_no_reclaim_step() {
    let mut world = machine_of(&[alice()]);

    let run = remove(&mut world, &alice(), &Script::default());

    let keys = run.step_keys();
    assert!(!keys.iter().any(|key| key == "reclaim"), "{keys:?}");
}
