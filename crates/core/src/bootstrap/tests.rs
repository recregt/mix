use std::path::Path;
use std::sync::Arc;

use mix_events::v1::{Cancellation, Command, NotRunReason, Status};
use mix_events::{Ending, Outbox, Outcome as EventOutcome, ROOT, Start, Tree, validate};

use super::*;
use crate::model::World;
use crate::plan::{Input, Next, Report, Runner, Verdict, diagnostic, make_guard};
use crate::privilege::InvokingUser;

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
    world
}

#[derive(Default, Clone, Copy)]
struct Script {
    fail_at: Option<usize>,
    stop_after: Option<usize>,
    fail_undo_at: Option<usize>,
}

struct Run {
    report: Report,
    stream: Vec<mix_events::v1::Envelope>,
    changes: usize,
    undos: usize,
}

fn run(world: &mut World, settings: &Settings, script: Script) -> Run {
    let outbox = Arc::new(Outbox::new("request", || {}));
    let mut tree = Tree::new(
        outbox.clone(),
        Arc::new(|| None),
        Start::command("bootstrap", Command::default()),
    );
    let mut runner = Runner::new(ROOT, steps(settings));
    make_guard!(guard);
    let mut runner = runner.brand(guard);
    let mut input = None;
    let mut changes = 0;
    let mut undos = 0;
    let report = loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(Ok(queries
                    .iter()
                    .map(|query| world.observe(query))
                    .collect())));
            }
            Next::Perform(action) => {
                let forward = !runner.rolling_back() && action != Action::Commit;
                let undoing = runner.rolling_back();
                let outcome = if (forward && script.fail_at == Some(changes))
                    || (undoing && script.fail_undo_at == Some(undos))
                {
                    Err(Failure::Io {
                        path: "/injected".into(),
                        kind: std::io::ErrorKind::Other,
                    })
                } else {
                    world.apply(&action)
                };
                if undoing {
                    undos += 1;
                }
                if forward {
                    if script.stop_after == Some(changes) {
                        runner.stop(Cancellation::Interrupted);
                    }
                    changes += 1;
                }
                input = Some(Input::Done(outcome));
            }
            Next::Finished(closed) => break runner.report(closed).clone(),
        }
    };
    let ending = match &report.verdict {
        Verdict::Succeeded => Ending::succeeded(),
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
    };
    tree.finish(ROOT, ending).unwrap();
    drop(tree);
    Run {
        report,
        stream: outbox.drain(),
        changes,
        undos,
    }
}

fn difference(left: &World, right: &World) -> String {
    let mut lines = Vec::new();
    for (path, entry) in &left.files {
        if right.files.get(path) != Some(entry) {
            lines.push(format!(
                "left has {}: {:?}",
                path.display(),
                entry.content_kind()
            ));
        }
    }
    for path in right.files.keys() {
        if !left.files.contains_key(path) {
            lines.push(format!("right has {}", path.display()));
        }
    }
    for (name, unit) in &left.units {
        if right.units.get(name).unwrap_or(&Default::default()) != unit {
            lines.push(format!(
                "unit {name}: {unit:?} vs {:?}",
                right.units.get(name)
            ));
        }
    }
    if left.groups != right.groups {
        lines.push(format!("groups {:?} vs {:?}", left.groups, right.groups));
    }
    if left.users.len() != right.users.len() {
        lines.push(format!(
            "{} users vs {}",
            left.users.len(),
            right.users.len()
        ));
    }
    if left.profiles != right.profiles {
        lines.push(format!(
            "profiles {:?} vs {:?}",
            left.profiles, right.profiles
        ));
    }
    if left.pending() != right.pending() {
        lines.push(format!(
            "pending {:?} vs {:?}",
            left.pending(),
            right.pending()
        ));
    }
    lines.join("\n")
}

fn contents(world: &World, path: &str) -> Option<String> {
    world
        .contents(path)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
}

#[test]
fn a_fresh_bootstrap_builds_the_whole_machine() {
    let mut world = machine();
    let settings = settings(None, false);

    let run = run(&mut world, &settings, Script::default());

    assert_eq!(run.report.verdict, Verdict::Succeeded);
    assert!(validate(&run.stream).is_ok());
    assert_eq!(world.files[Path::new("/nix")].mode, 0o755);
    assert!(world.contents(NIX_OWNERSHIP_MARKER).is_some());
    for path in NIX_TREE_PATHS {
        assert!(world.files.contains_key(Path::new(path)), "{path}");
    }
    assert_eq!(world.files[Path::new(NIXBLD_HOME)].mode, NIXBLD_HOME_MODE);
    assert_eq!(world.groups[NIXBLD_GROUP].gid, NIXBLD_GID);
    assert_eq!(world.groups[MIX_USERS_GROUP].gid, MIX_USERS_GID);
    assert_eq!(world.users.len(), NIXBLD_USER_COUNT as usize);
    assert_eq!(world.users["nixbld7"].uid, NIXBLD_UID_BASE + 7);
    assert_eq!(
        world.groups[NIXBLD_GROUP].members.len(),
        NIXBLD_USER_COUNT as usize
    );
    assert_eq!(
        contents(&world, NIX_CONF_DEST),
        Some(settings.policy.nix_conf().to_string())
    );
    assert_eq!(
        contents(&world, POLICY_FILE),
        Some(settings.policy.render().to_string())
    );
    let Fact::Unit(unit) = world.observe(&Query::Unit(NIX_DAEMON_SOCKET_UNIT.into())) else {
        panic!("a unit query is answered with unit facts");
    };
    assert_eq!(unit.load_state, "loaded");
    assert_eq!(unit.active_state, "active");
    assert!(unit.enabled());
    assert!(!unit.needs_reload);
    assert!(world.pending().is_empty());
}

#[test]
fn a_second_bootstrap_changes_nothing() {
    let mut world = machine();
    let settings = settings(Some(alice()), false);
    run(&mut world, &settings, Script::default());
    let before = world.clone();

    let again = run(&mut world, &settings, Script::default());

    assert_eq!(again.report.verdict, Verdict::Succeeded);
    assert_eq!(again.changes, 0);
    assert_eq!(world, before);
    let validated = validate(&again.stream).unwrap();
    for step in steps(&settings) {
        assert_eq!(
            validated.outcome(&format!("bootstrap/plan/{}", step.key())),
            Some(EventOutcome::Finished(Status::AlreadySatisfied)),
            "{}",
            step.key()
        );
    }
}

#[test]
fn a_bootstrap_with_a_user_writes_their_files_as_theirs_and_enrols_them() {
    let mut world = machine();
    let user = alice();

    let run = run(
        &mut world,
        &settings(Some(user.clone()), false),
        Script::default(),
    );

    assert_eq!(run.report.verdict, Verdict::Succeeded);
    let state = mix_state_dir(&user.user.home);
    assert_eq!(world.files[&state].mode, MIX_STATE_DIR_MODE);
    for file in [HOME_NIX, FLAKE_NIX, FLAKE_LOCK, STATE_FILE] {
        assert_eq!(world.files[&state.join(file)].owner, (1000, 1000), "{file}");
    }
    assert_eq!(
        contents(&world, state.join(STATE_FILE).to_str().unwrap()),
        Some(StateManifest::seed_rendered().to_string())
    );
    assert!(
        world.groups[MIX_USERS_GROUP]
            .members
            .contains(&"alice".to_string())
    );
    assert_eq!(world.profile(&user.user).and_then(|p| p.active), Some(1));
    assert!(world.files.contains_key(&state.join(".git")));
}

#[test]
fn a_failure_at_any_change_leaves_the_machine_as_it_was() {
    let base = machine();
    let settings = settings(Some(alice()), false);
    let changes = run(&mut base.clone(), &settings, Script::default()).changes;

    for fail_at in 0..changes {
        let mut world = base.clone();

        let run = run(
            &mut world,
            &settings,
            Script {
                fail_at: Some(fail_at),
                ..Script::default()
            },
        );

        assert!(
            matches!(run.report.verdict, Verdict::Failed { .. }),
            "{fail_at}"
        );
        assert!(
            run.report.rollback_failures.is_empty(),
            "{fail_at}: {:?}",
            run.report.rollback_failures
        );
        assert!(
            world == base,
            "failing at change {fail_at}:\n{}",
            difference(&world, &base)
        );
        assert!(validate(&run.stream).is_ok());
    }
}

#[test]
fn an_undo_that_fails_is_reported_and_every_other_undo_still_runs() {
    let base = machine();
    let settings = settings(Some(alice()), false);
    let last = run(&mut base.clone(), &settings, Script::default()).changes - 1;
    let failing_last = Script {
        fail_at: Some(last),
        ..Script::default()
    };
    let undos = run(&mut base.clone(), &settings, failing_last).undos;

    for fail_undo_at in 0..undos {
        let run = run(
            &mut base.clone(),
            &settings,
            Script {
                fail_undo_at: Some(fail_undo_at),
                ..failing_last
            },
        );

        assert!(
            matches!(run.report.verdict, Verdict::Failed { .. }),
            "{fail_undo_at}"
        );
        assert!(!run.report.rollback_failures.is_empty(), "{fail_undo_at}");
        assert!(
            run.undos >= undos,
            "{fail_undo_at}: {} of {undos}",
            run.undos
        );
        assert!(validate(&run.stream).is_ok(), "{fail_undo_at}");
        assert!(
            run.stream.iter().any(|envelope| matches!(
                &envelope.event,
                Some(mix_events::v1::envelope::Event::NodeFinished(finished))
                    if finished.diagnostic.as_ref().is_some_and(|diagnostic| {
                        diagnostic.code == mix_events::v1::Code::RollbackIncomplete as i32
                    })
            )),
            "{fail_undo_at}"
        );
    }
}

#[test]
fn a_stop_after_any_change_leaves_the_machine_as_it_was() {
    let base = machine();
    let settings = settings(Some(alice()), false);
    let changes = run(&mut base.clone(), &settings, Script::default()).changes;

    for stop_after in 0..changes {
        let mut world = base.clone();

        let run = run(
            &mut world,
            &settings,
            Script {
                stop_after: Some(stop_after),
                ..Script::default()
            },
        );

        assert_eq!(
            run.report.verdict,
            Verdict::Cancelled(Cancellation::Interrupted),
            "{stop_after}"
        );
        assert!(
            world == base,
            "stopping after change {stop_after}:\n{}",
            difference(&world, &base)
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
    assert_eq!(forced.report.verdict, Verdict::Succeeded);
    assert_eq!(
        contents(&replaced, NIX_CONF_DEST),
        Some(settings.policy.nix_conf().to_string())
    );
    assert!(replaced.pending().is_empty());

    for fail_at in [0, changes / 2, changes - 1] {
        let mut world = installed.clone();
        let failed = run(
            &mut world,
            &settings,
            Script {
                fail_at: Some(fail_at),
                ..Script::default()
            },
        );
        assert!(matches!(failed.report.verdict, Verdict::Failed { .. }));
        assert!(
            failed.report.rollback_failures.is_empty(),
            "{:?}",
            failed.report.rollback_failures
        );
        assert!(
            world == installed,
            "failing at change {fail_at} of {changes}:\n{}",
            difference(&world, &installed)
        );
    }
}

#[test]
fn a_file_where_nix_belongs_is_refused_and_nothing_is_touched() {
    let mut world = machine();
    world.with_file("/nix", b"not a directory", 0o644, (0, 0));
    let before = world.clone();

    let run = run(&mut world, &settings(None, false), Script::default());

    assert!(matches!(
        &run.report.verdict,
        Verdict::Failed {
            step,
            failure: Failure::Conflict { .. }
        } if step == "create-nix-dir"
    ));
    assert_eq!(world, before);
    assert_eq!(
        validate(&run.stream)
            .unwrap()
            .outcome("bootstrap/plan/create-nix-tree"),
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

    assert_eq!(rewritten.report.verdict, Verdict::Succeeded);
    assert!(world.units[NIX_DAEMON_SERVICE_UNIT].since > Some(started));
}

#[test]
fn a_masked_socket_is_left_alone_and_reported() {
    let facts = [
        Fact::Contents(Some(Arc::from(&b"[Service]"[..]))),
        Fact::Path(PathFacts {
            kind: Kind::File,
            mode: 0o644,
            owner: (0, 0),
            id: None,
            digest: None,
            changed: None,
        }),
        Fact::Contents(Some(Arc::from(&b"[Service]"[..]))),
        Fact::Contents(Some(Arc::from(&b"[Socket]"[..]))),
        Fact::Path(PathFacts {
            kind: Kind::File,
            mode: 0o644,
            owner: (0, 0),
            id: None,
            digest: None,
            changed: None,
        }),
        Fact::Contents(Some(Arc::from(&b"[Socket]"[..]))),
        Fact::Unit(UnitFacts {
            load_state: "masked".into(),
            active_state: "inactive".into(),
            file_state: "masked".into(),
            needs_reload: false,
            active_since: None,
        }),
        Fact::Unit(UnitFacts {
            load_state: "loaded".into(),
            active_state: "inactive".into(),
            file_state: "static".into(),
            needs_reload: false,
            active_since: None,
        }),
        Fact::Path(PathFacts {
            kind: Kind::File,
            mode: 0o644,
            owner: (0, 0),
            id: None,
            digest: None,
            changed: Some((5, 0)),
        }),
    ];

    assert!(matches!(
        ConfigureDaemon.actions(&facts),
        Err(Failure::Conflict { .. })
    ));
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

    let again = run(&mut world, &settings, Script::default());

    assert_eq!(again.report.verdict, Verdict::Succeeded);
    assert!(again.changes > 0);
    assert!(!matches!(
        world.observe(&Query::Path(profile)),
        Fact::Path(PathFacts {
            kind: Kind::Missing,
            ..
        })
    ));
    assert!(validate(&again.stream).is_ok());
}

#[test]
fn every_step_is_named_in_the_users_words() {
    for step in steps(&settings(Some(alice()), true)) {
        let subject = step.title().subject;
        assert_eq!(
            crate::vocabulary::nix_mechanics_in(&subject),
            Vec::<&str>::new(),
            "{subject}"
        );
    }
}
