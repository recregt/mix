use std::path::Path;

use mix_events::ROOT;
use mix_events::v1::PurgeRequest;
use mix_events::v1::command::Request;

use super::*;
use crate::declared::identity::NIXBLD_USER_COUNT;
use crate::declared::paths::{INSTALLED_PROGRAMS, MIX_STATE_DIR, NIX_STORE, mix_state_dir};
use crate::model::World;
use crate::model::testkit::{
    Run, Script, account, difference, drive, enrolled, recover, requested,
};
use crate::run::{Runner, Verdict};

fn alice() -> InvokingUser {
    account("alice", 1000)
}

fn bootstrapped() -> World {
    let mut world = enrolled(&[alice()]);
    world.with_file("/home/alice/.bashrc", b"linked", 0o644, (1000, 1000));
    world.with_file("/home/alice/notes", b"mine", 0o644, (1000, 1000));
    world
        .linked
        .insert(1000, vec!["/home/alice/.bashrc".into()]);
    world
}

fn remove(world: &mut World, script: &Script) -> Run {
    drive(
        world,
        Runner::new(
            ROOT,
            steps(&Settings {
                users: vec![alice()],
            }),
        ),
        requested(Request::Purge(PurgeRequest {})),
        script,
    )
}

fn present(world: &World, path: &str) -> bool {
    world.files.contains_key(Path::new(path))
}

#[test]
fn a_purge_leaves_nothing_mix_made_and_keeps_the_users_own_files() {
    let mut world = bootstrapped();
    assert!(present(&world, NIX_STORE));
    assert!(present(
        &world,
        &mix_state_dir(&alice().home).to_string_lossy()
    ));

    let run = remove(&mut world, &Script::default());

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert!(world.pending().is_empty(), "{:?}", world.pending());
    for gone in [
        "/nix/store",
        "/nix/var",
        "/etc/nix/nix.conf",
        "/etc/profile.d/mix-nix.sh",
        "/etc/mix",
        "/var/lib/mix/bin",
        "/etc/systemd/system/nix-daemon.socket",
        "/etc/systemd/system/mix-daemon.service",
        "/home/alice/.bashrc",
    ] {
        assert!(!present(&world, gone), "{gone} is still there");
    }
    assert!(!present(&world, &format!("/home/alice/{MIX_STATE_DIR}")));
    assert!(present(&world, "/home/alice/notes"));
    assert!(present(&world, "/home/alice"));
    for n in 1..=NIXBLD_USER_COUNT {
        assert!(!world.users.contains_key(&*user_name(n)));
    }
    assert!(!world.groups.contains_key(NIXBLD_GROUP));
    assert!(!world.groups.contains_key(MIX_USERS_GROUP));
    for unit in UNITS {
        let facts = world.units.get(unit).cloned().unwrap_or_default();
        assert!(!facts.enabled, "{unit} is still enabled");
        assert!(facts.running.is_none(), "{unit} is still running");
    }
    assert!(world.users.contains_key("alice"));
}

#[test]
fn every_home_is_set_aside_before_anything_on_the_machine_changes() {
    let mut world = bootstrapped();

    let run = remove(&mut world, &Script::default());

    let last_home = run
        .performed
        .iter()
        .rposition(|action| {
            matches!(action, Action::SetAside { path, .. } if path.starts_with("/home/alice"))
        })
        .expect("alice's home was cleaned");
    let first_machine = run
        .performed
        .iter()
        .position(|action| match action {
            Action::SetAside { path, .. } => !path.starts_with("/home"),
            Action::StopUnit { .. } | Action::DisableUnit { .. } => true,
            _ => false,
        })
        .expect("the machine was cleaned");
    assert!(last_home < first_machine, "{:#?}", run.performed);
}

#[test]
fn a_failure_before_the_commit_puts_everything_back() {
    let mut world = bootstrapped();
    let before = world.clone();
    let accounts = |action: &Action| matches!(action, Action::DeleteGroup { .. });

    let run = remove(
        &mut world,
        &Script {
            fail_when: Some(accounts),
            ..Script::default()
        },
    );

    assert!(matches!(run.report().verdict, Verdict::Failed { .. }));
    assert_eq!(difference(&before, &world), serde_json::json!({}));
}

#[test]
fn the_nix_store_is_never_moved_or_copied_and_only_goes_at_the_commit() {
    let mut world = bootstrapped();
    let store = world.files[Path::new(NIX_STORE)].id;
    let accounts = |action: &Action| matches!(action, Action::DeleteGroup { .. });

    let failed = remove(
        &mut world,
        &Script {
            fail_when: Some(accounts),
            ..Script::default()
        },
    );

    assert!(failed.performed.contains(&Action::Discard {
        path: NIX_STORE.into(),
        expect: store,
    }));
    assert!(!failed.performed.iter().any(|action| matches!(
        action,
        Action::SetAside { path, .. } | Action::CopyTree { from: path, .. }
            if path.starts_with("/nix")
    )));
    assert_eq!(world.files[Path::new(NIX_STORE)].id, store);
    assert!(world.pending().is_empty(), "{:?}", world.pending());

    let removed = remove(&mut world, &Script::default());

    assert_eq!(removed.report().verdict, Verdict::Succeeded);
    assert!(!present(&world, NIX_STORE));
}

#[test]
fn a_linked_file_inside_a_removed_directory_is_not_set_aside_twice() {
    let mut world = bootstrapped();
    let inner = format!("/home/alice/{MIX_STATE_DIR}/home.nix");
    world.linked.insert(1000, vec![inner.clone().into()]);

    let run = remove(&mut world, &Script::default());

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert!(!run.performed.iter().any(
        |action| matches!(action, Action::SetAside { path, .. } if path == Path::new(&inner))
    ));
}

#[test]
fn the_installed_programs_are_left_to_the_final_cleanup() {
    let mut world = bootstrapped();
    for program in INSTALLED_PROGRAMS {
        world.with_file(program, b"mix", 0o755, (0, 0));
    }

    remove(&mut world, &Script::default());

    for program in INSTALLED_PROGRAMS {
        assert!(present(&world, program), "{program} was removed by a step");
    }
}

#[test]
fn a_crash_at_any_action_ends_as_either_the_old_machine_or_a_finished_removal() {
    let before = bootstrapped();
    let mut finished = before.clone();
    remove(&mut finished, &Script::default());
    let mut crash_at = 0;
    loop {
        let mut tried = false;
        for after in [false, true] {
            let mut world = before.clone();
            let crashed = remove(&mut world, &Script::crashing(crash_at, after));
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
    assert!(crash_at > 40, "too few actions to crash in: {crash_at}");
}

#[test]
fn deleting_the_store_is_its_own_step_so_the_wait_is_visible() {
    let mut world = bootstrapped();

    let run = remove(&mut world, &Script::default());

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert_eq!(run.step_keys().last().map(String::as_str), Some("reclaim"));
}

fn facts(members: &[&str], accounts: &[(&str, u32, u32)], login_defs: Option<&str>) -> Vec<Fact> {
    let accounts = accounts
        .iter()
        .map(|(name, uid, gid)| {
            (
                (*name).to_string(),
                crate::effect::UserFacts {
                    uid: *uid,
                    gid: *gid,
                    home: format!("/home/{name}").into(),
                    shell: "/bin/sh".into(),
                    comment: String::new(),
                },
            )
        })
        .collect();
    vec![
        Fact::Group(Some(crate::effect::GroupFacts {
            gid: 990,
            members: members.iter().map(|name| (*name).to_string()).collect(),
        })),
        Fact::Group(Some(crate::effect::GroupFacts {
            gid: 30000,
            members: Vec::new(),
        })),
        Fact::Accounts(accounts),
        Fact::Contents(login_defs.map(|text| std::sync::Arc::from(text.as_bytes()))),
    ]
}

fn names(users: &[InvokingUser]) -> Vec<&str> {
    users.iter().map(|user| user.name.as_str()).collect()
}

fn state(id: Option<crate::effect::FileId>) -> Fact {
    Fact::Path(crate::effect::PathFacts {
        kind: if id.is_some() {
            crate::effect::Kind::Directory
        } else {
            crate::effect::Kind::Missing
        },
        mode: 0o700,
        owner: (1001, 1001),
        id,
        digest: None,
        changed: None,
    })
}

fn kept() -> Fact {
    state(Some(crate::effect::FileId {
        dev: 1,
        ino: 1,
        born: None,
    }))
}

fn gone() -> Fact {
    state(None)
}

#[test]
fn members_always_hold_and_a_former_user_holds_only_while_their_state_is_there() {
    let candidates = Candidates::of(&facts(
        &["alice"],
        &[
            ("root", 0, 0),
            ("alice", 1000, 1000),
            ("bob", 1001, 1001),
            ("carol", 1002, 1002),
            ("nixbld1", 30001, 30000),
            ("nobody", 65534, 65534),
        ],
        None,
    ));

    assert_eq!(
        candidates.queries(),
        [
            Query::Path(mix_state_dir(Path::new("/home/bob"))),
            Query::Path(mix_state_dir(Path::new("/home/carol"))),
        ]
    );
    assert_eq!(
        names(&candidates.holders(&[kept(), gone()])),
        ["alice", "bob"]
    );
}

#[test]
fn the_regular_range_comes_from_login_defs_and_otherwise_the_shadow_defaults() {
    let accounts = [("old", 500, 500), ("alice", 1000, 1000)];

    let defaults = Candidates::of(&facts(&[], &accounts, None));
    let widened = Candidates::of(&facts(&[], &accounts, Some("# shadow\nUID_MIN\t 500\n")));

    assert_eq!(defaults.queries().len(), 1);
    assert_eq!(widened.queries().len(), 2);
}

#[test]
fn a_member_with_no_account_left_is_not_a_holder() {
    let candidates = Candidates::of(&facts(&["ghost"], &[("alice", 1000, 1000)], None));

    assert_eq!(names(&candidates.holders(&[gone()])), Vec::<&str>::new());
}
