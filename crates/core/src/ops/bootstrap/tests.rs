use std::path::Path;
use std::sync::Arc;

use insta::assert_json_snapshot;
use mix_events::v1::command::Request;
use mix_events::v1::{BootstrapRequest, Cancellation, NotRunReason};
use mix_events::{Outcome as EventOutcome, ROOT, validate};

use super::*;
use crate::declared::identity::InvokingUser;
use crate::declared::paths::{DEFAULT_PROFILE_NIX_ENV, MIX_DAEMON_SERVICE_UNIT};
use crate::effect::{Digest, Kind};
use crate::model::World;
use crate::model::testkit::{Run, Script, difference, drive, requested};
use crate::run::{Runner, Verdict};

fn settings(user: Option<UserConfig>, force: bool) -> Settings {
    Settings {
        policy: Policy::new(Some("https://mirror.internal"), Some("mirror:AAAA")).unwrap(),
        user,
        force,
        runtime: Runtime {
            url: "https://mirror.internal/nix-2.35.2-x86_64-linux.tar.xz".into(),
            sha256: Digest([7; 32]),
            size: 27_131_728,
        },
        request: "request-1".into(),
    }
}

fn alice() -> UserConfig {
    UserConfig {
        user: InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".into(),
            home: "/home/alice".into(),
        },
        flake: "flake".into(),
        lock: "lock".into(),
        home: "home".into(),
        restored_state: None,
    }
}

fn machine() -> World {
    let mut world = World::default();
    world.with_dir("/home/alice", 0o700, (1000, 1000));
    world.users.insert(
        "alice".into(),
        crate::effect::UserFacts {
            uid: 1000,
            gid: 1000,
            home: "/home/alice".into(),
            shell: "/bin/sh".into(),
            comment: String::new(),
        },
    );
    world.with_file(
        crate::declared::paths::RUNNING_PROGRAM,
        b"mix-daemon",
        0o755,
        (0, 0),
    );
    world
}

fn run(world: &mut World, settings: &Settings, script: Script) -> Run {
    let request = Request::Bootstrap(Box::new(BootstrapRequest {
        force: settings.force,
        ..BootstrapRequest::default()
    }));
    drive(
        world,
        Runner::new(ROOT, steps(settings)),
        requested(request),
        &script,
    )
}

#[test]
fn a_fresh_bootstrap_builds_the_whole_machine() {
    let mut world = machine();
    let before = world.clone();

    let run = run(&mut world, &settings(None, false), Script::default());

    assert_json_snapshot!(run.case(&before, &world));
    assert!(world.pending().is_empty());
}

#[test]
fn a_second_bootstrap_changes_nothing() {
    let mut world = machine();
    let settings = settings(Some(alice()), false);
    run(&mut world, &settings, Script::default());
    let before = world.clone();

    let again = run(&mut world, &settings, Script::default());

    assert_json_snapshot!(again.case(&before, &world));
    assert_eq!(world, before);
}

#[test]
fn a_bootstrap_with_a_user_writes_their_files_as_theirs_and_enrols_them() {
    let mut world = machine();
    let before = world.clone();

    let run = run(
        &mut world,
        &settings(Some(alice()), false),
        Script::default(),
    );

    assert_json_snapshot!(run.case(&before, &world));
}

#[test]
fn a_failure_at_any_change_leaves_the_machine_as_it_was() {
    let base = machine();
    let settings = settings(Some(alice()), false);
    let changes = run(&mut base.clone(), &settings, Script::default()).changes;

    for fail_at in 0..changes {
        let mut world = base.clone();

        let run = run(&mut world, &settings, Script::failing_at(fail_at));

        assert!(
            matches!(run.report().verdict, Verdict::Failed { .. }),
            "{fail_at}"
        );
        assert!(
            run.report().rollback_failures.is_empty(),
            "{fail_at}: {:?}",
            run.report().rollback_failures
        );
        assert!(
            world == base,
            "failing at change {fail_at}:\n{:#}",
            difference(&base, &world)
        );
    }
}

#[test]
fn an_undo_that_fails_is_reported_and_every_other_undo_still_runs() {
    let base = machine();
    let settings = settings(Some(alice()), false);
    let last = run(&mut base.clone(), &settings, Script::default()).changes - 1;
    let failing_last = Script::failing_at(last);
    let undos = run(&mut base.clone(), &settings, failing_last.clone()).undos;

    for fail_undo_at in 0..undos {
        let run = run(
            &mut base.clone(),
            &settings,
            Script {
                fail_undo_at: Some(fail_undo_at),
                ..failing_last.clone()
            },
        );

        assert!(
            matches!(run.report().verdict, Verdict::Failed { .. }),
            "{fail_undo_at}"
        );
        assert!(!run.report().rollback_failures.is_empty(), "{fail_undo_at}");
        assert!(
            run.undos >= undos,
            "{fail_undo_at}: {} of {undos}",
            run.undos
        );
        assert!(
            run.stream.iter().any(|envelope| matches!(
                &envelope.event,
                Some(mix_events::v1::envelope::Event::Diagnostic(diagnostic))
                    if diagnostic.code == mix_events::v1::Code::RollbackIncomplete as i32
            )),
            "{fail_undo_at}"
        );
    }
}

#[test]
fn a_stop_after_any_change_leaves_the_machine_as_it_was_unless_only_the_record_is_left() {
    let base = machine();
    let settings = settings(Some(alice()), false);
    let finished = run(&mut base.clone(), &settings, Script::default());
    let record = finished
        .performed
        .iter()
        .filter(|action| **action != Action::Commit)
        .position(|action| matches!(action, Action::RecordState { .. }));

    for stop_after in 0..finished.changes {
        let mut world = base.clone();

        let run = run(&mut world, &settings, Script::stopping_after(stop_after));

        if record == Some(stop_after) {
            assert_eq!(run.report().verdict, Verdict::Succeeded, "{stop_after}");
            continue;
        }
        assert_eq!(
            run.report().verdict,
            Verdict::Cancelled(Cancellation::Interrupted),
            "{stop_after}"
        );
        assert!(
            world == base,
            "stopping after change {stop_after}:\n{:#}",
            difference(&base, &world)
        );
    }
}

#[test]
fn forcing_over_an_installation_replaces_it_and_a_failure_brings_it_back() {
    let mut installed = machine();
    run(
        &mut installed,
        &settings(Some(alice()), false),
        Script::default(),
    );
    installed.with_file(
        NIX_CONF_DEST,
        b"trusted-users = root alice\n",
        0o644,
        (0, 0),
    );
    let settings = settings(Some(alice()), true);
    let changes = run(&mut installed.clone(), &settings, Script::default()).changes;

    let mut replaced = installed.clone();
    let forced = run(&mut replaced, &settings, Script::default());
    assert_json_snapshot!(forced.case(&installed, &replaced));
    assert!(replaced.pending().is_empty());

    for fail_at in [0, changes / 2, changes - 1] {
        let mut world = installed.clone();
        let failed = run(&mut world, &settings, Script::failing_at(fail_at));
        assert!(matches!(failed.report().verdict, Verdict::Failed { .. }));
        assert!(
            failed.report().rollback_failures.is_empty(),
            "{:?}",
            failed.report().rollback_failures
        );
        assert!(
            world == installed,
            "failing at change {fail_at} of {changes}:\n{:#}",
            difference(&installed, &world)
        );
    }
}

#[test]
fn a_file_where_nix_belongs_is_refused_and_nothing_is_touched() {
    let mut world = machine();
    world.with_file("/nix", b"not a directory", 0o644, (0, 0));
    let before = world.clone();

    let run = run(&mut world, &settings(None, false), Script::default());

    assert_json_snapshot!(run.case(&before, &world));
    assert_eq!(world, before);
    assert_eq!(
        validate(&run.stream)
            .unwrap()
            .outcome("bootstrap/plan//nix/store"),
        Some(EventOutcome::NotRun(NotRunReason::NotReached))
    );
}

#[test]
fn a_running_daemon_is_restarted_only_when_its_configuration_changed_after_it_started() {
    let mut world = machine();
    run(&mut world, &settings(None, false), Script::default());
    let started = world.now();
    let service = world
        .units
        .entry(NIX_DAEMON_SERVICE_UNIT.into())
        .or_default();
    service.loaded = service
        .loaded
        .clone()
        .or(Some(Arc::from(&b"[Service]"[..])));
    service.running = service.loaded.clone();
    service.since = Some(started);

    let unchanged = run(&mut world, &settings(None, false), Script::default());
    assert_eq!(unchanged.changes, 0);
    assert_eq!(world.units[NIX_DAEMON_SERVICE_UNIT].since, Some(started));

    let mut other = settings(None, false);
    other.policy = Policy::new(Some("https://elsewhere.internal"), None).unwrap();
    let rewritten = run(&mut world, &other, Script::default());

    assert_eq!(rewritten.report().verdict, Verdict::Succeeded);
    assert!(world.units[NIX_DAEMON_SERVICE_UNIT].since > Some(started));
}

#[test]
fn a_new_daemon_binary_asks_the_running_daemon_to_drain_instead_of_restarting_it() {
    let mut world = machine();
    run(&mut world, &settings(None, false), Script::default());
    let service = world
        .units
        .entry(MIX_DAEMON_SERVICE_UNIT.into())
        .or_default();
    service.running = service
        .loaded
        .clone()
        .or(Some(Arc::from(&b"[Service]"[..])));
    world.with_file(
        crate::declared::paths::RUNNING_PROGRAM,
        b"mix-daemon 2",
        0o755,
        (0, 0),
    );
    let before = world.clone();

    let upgraded = run(&mut world, &settings(None, false), Script::default());

    assert_json_snapshot!(upgraded.case(&before, &world));
}

#[test]
fn a_runtime_whose_default_profile_was_lost_is_provisioned_again() {
    let mut world = machine();
    let settings = settings(Some(alice()), false);
    run(&mut world, &settings, Script::default());
    let profile = std::path::PathBuf::from(DEFAULT_PROFILE_NIX_ENV);
    let default = profile
        .parent()
        .and_then(Path::parent)
        .expect("the default profile holds bin/nix-env")
        .to_path_buf();
    let Fact::Path(PathFacts { id: Some(id), .. }) = world.observe(&Query::Path(default.clone()))
    else {
        panic!("a bootstrap installs the default profile");
    };
    world
        .apply(&Action::RemoveCreatedTree {
            path: default,
            expect: id,
        })
        .unwrap();
    assert!(matches!(
        world.observe(&Query::Path(profile.clone())),
        Fact::Path(PathFacts {
            kind: Kind::Missing,
            ..
        })
    ));

    let before = world.clone();

    let again = run(&mut world, &settings, Script::default());

    assert_json_snapshot!(again.case(&before, &world));
}

#[test]
fn every_step_is_named_in_the_users_words() {
    for step in steps(&settings(Some(alice()), true)) {
        let subject = step.title().subject;
        if subject.starts_with('/') {
            continue;
        }
        assert_eq!(
            crate::report::vocabulary::nix_mechanics_in(&subject),
            Vec::<&str>::new(),
            "{subject}"
        );
    }
}
