use std::path::PathBuf;
use std::sync::Arc;

use mix_events::v1::Command;
use mix_events::{Outbox, ROOT, Start, Tree};

use super::*;
use crate::action::Digest;
use crate::action::{Expect, Kind};
use crate::bootstrap::{Runtime, Settings};
use crate::journal::{Record, Recovery, recover};
use crate::plan::{Input, Next, Report, Runner, Verdict, make_guard};
use crate::policy::Policy;
use crate::targets::UserConfig;
use crate::world::World;
use mix_events::v1::Cancellation;

fn manifest(packages: &[&str]) -> StateManifest {
    StateManifest {
        version: STATE_VERSION,
        packages: packages.iter().map(|p| p.to_string()).collect(),
    }
}

fn current(manifest: StateManifest, source: Source) -> Settled {
    Settled::Current { manifest, source }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

fn user() -> InvokingUser {
    InvokingUser {
        uid: 1000,
        gid: 1000,
        name: "mix-user".to_string(),
        home: PathBuf::from("/home/mix-user"),
    }
}

#[test]
fn the_seed_is_valid() {
    assert_eq!(
        validate(&StateManifest::seed().render()),
        Ok(StateManifest::seed())
    );
}

#[test]
fn a_list_mix_rendered_is_valid() {
    let list = manifest(&["git", "ripgrep", "node-sass"]);
    assert_eq!(validate(&list.render()), Ok(list));
}

#[test]
fn broken_json_is_invalid() {
    assert!(matches!(validate("{broken"), Err(Invalid::Unreadable(_))));
    assert!(matches!(validate(""), Err(Invalid::Unreadable(_))));
}

#[test]
fn a_newer_format_is_told_apart_from_an_unknown_one() {
    assert_eq!(
        validate(r#"{"version":2,"packages":["git"]}"#),
        Err(Invalid::Newer(2))
    );
    assert_eq!(
        validate(r#"{"version":0,"packages":["git"]}"#),
        Err(Invalid::UnknownVersion(0))
    );
}

#[test]
fn a_name_nix_cannot_read_is_invalid() {
    assert_eq!(
        validate(r#"{"version":1,"packages":["git","rm -rf"]}"#),
        Err(Invalid::Package("rm -rf".to_string()))
    );
    assert_eq!(
        validate(r#"{"version":1,"packages":["git","with"]}"#),
        Err(Invalid::Package("with".to_string()))
    );
}

#[test]
fn a_list_without_git_is_invalid() {
    assert_eq!(
        validate(r#"{"version":1,"packages":["ripgrep"]}"#),
        Err(Invalid::Missing("git"))
    );
}

#[test]
fn a_valid_file_with_no_copy_in_the_profile_is_kept() {
    let file = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(Some(&file), None),
        current(manifest(&["git", "hello"]), Source::File)
    );
}

#[test]
fn a_valid_file_matching_the_profile_is_kept() {
    let list = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(Some(&list), Some(&list)),
        current(manifest(&["git", "hello"]), Source::File)
    );
}

#[test]
fn a_file_the_profile_never_switched_to_gives_way_to_the_profile() {
    let file = manifest(&["git", "hello"]).render();
    let generation = manifest(&["git"]).render();

    assert_eq!(
        settle(Some(&file), Some(&generation)),
        current(manifest(&["git"]), Source::Generation)
    );
}

#[test]
fn a_broken_file_is_restored_from_the_profile() {
    let generation = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(Some("{broken"), Some(&generation)),
        current(manifest(&["git", "hello"]), Source::Generation)
    );
}

#[test]
fn a_missing_file_is_restored_from_the_profile() {
    let generation = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(None, Some(&generation)),
        current(manifest(&["git", "hello"]), Source::Generation)
    );
}

#[test]
fn with_nothing_to_restore_from_a_fresh_list_is_started() {
    assert_eq!(
        settle(Some("{broken"), None),
        current(StateManifest::seed(), Source::Fresh)
    );
    assert_eq!(
        settle(None, None),
        current(StateManifest::seed(), Source::Fresh)
    );
}

#[test]
fn a_broken_copy_in_the_profile_is_never_restored() {
    assert_eq!(
        settle(Some("{broken"), Some("{also broken")),
        current(StateManifest::seed(), Source::Fresh)
    );
}

#[test]
fn a_valid_file_is_kept_over_a_broken_copy_in_the_profile() {
    let file = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(Some(&file), Some("{broken")),
        current(manifest(&["git", "hello"]), Source::File)
    );
}

#[test]
fn a_list_from_a_newer_mix_is_left_alone() {
    let generation = manifest(&["git"]).render();

    assert_eq!(
        settle(
            Some(r#"{"version":2,"packages":["git"]}"#),
            Some(&generation)
        ),
        Settled::Newer(2)
    );
}

#[test]
fn a_same_list_written_differently_still_matches_the_profile() {
    let generation = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(
            Some(r#"{"version":1,"packages":["git","hello"]}"#),
            Some(&generation)
        ),
        current(manifest(&["git", "hello"]), Source::File)
    );
}

#[test]
fn an_install_appends_what_is_missing_and_skips_what_is_there() {
    let request = names(&["git", "ripgrep"]);

    let change = install(&request, current(manifest(&["git"]), Source::File)).unwrap();

    assert_eq!(change.changed, names(&["ripgrep"]));
    assert_eq!(change.skipped, names(&["git"]));
    assert_eq!(change.manifest, manifest(&["git", "ripgrep"]));
    assert_eq!(change.source, Source::File);
}

#[test]
fn a_repeated_package_is_counted_once() {
    let request = names(&["git", "git", "fd", "fd"]);

    let change = install(&request, current(manifest(&["git"]), Source::File)).unwrap();

    assert_eq!(change.changed, names(&["fd"]));
    assert_eq!(change.skipped, names(&["git"]));
    assert_eq!(change.manifest, manifest(&["fd", "git"]));
}

#[test]
fn an_install_keeps_the_list_version() {
    let request = names(&["fd"]);
    let settled = StateManifest {
        version: 7,
        packages: names(&["git"]),
    };

    let change = install(&request, current(settled, Source::File)).unwrap();

    assert_eq!(change.manifest.version, 7);
}

#[test]
fn a_remove_drops_what_is_there_and_skips_what_is_not() {
    let request = names(&["ripgrep", "fd"]);

    let change = remove(
        &request,
        current(manifest(&["git", "ripgrep"]), Source::Generation),
    )
    .unwrap();

    assert_eq!(change.changed, names(&["ripgrep"]));
    assert_eq!(change.skipped, names(&["fd"]));
    assert_eq!(change.manifest, manifest(&["git"]));
    assert_eq!(change.source, Source::Generation);
}

#[test]
fn a_protected_package_is_refused_before_the_list_is_read() {
    let request = names(&["git", "ripgrep"]);

    assert_eq!(
        remove(&request, Settled::Newer(9)),
        Err(Refusal::Protected(names(&["git"])))
    );
}

#[test]
fn a_list_from_a_newer_mix_is_never_changed() {
    let request = names(&["ripgrep"]);

    assert_eq!(install(&request, Settled::Newer(2)), Err(NewerList(2)));
}

#[test]
fn nothing_changes_when_every_package_is_already_there() {
    let request = names(&["git"]);

    let change = install(&request, current(manifest(&["git"]), Source::File)).unwrap();

    assert!(change.changed.is_empty());
    assert_eq!(change.manifest, manifest(&["git"]));
}

#[test]
fn rendering_writes_every_package_into_the_list_and_home_nix() {
    let rendered = render(&user(), &manifest(&["git", "ripgrep"])).unwrap();

    assert_eq!(
        StateManifest::parse(&rendered.state).unwrap().packages,
        names(&["git", "ripgrep"])
    );
    assert!(rendered.home_nix.contains("ripgrep"));
    assert!(rendered.home_nix.contains("git"));
}

#[test]
fn rendering_copies_every_input_into_the_generation() {
    let rendered = render(&user(), &manifest(&["git"])).unwrap();

    assert!(rendered.home_nix.contains(
        "extraBuilderCommands = \"cp ${./state} $out/mix-state\n\
         cp ${./flake.nix} $out/mix-flake.nix\n\
         cp ${./flake.lock} $out/mix-flake.lock\n\
         cp ${./home.nix} $out/mix-home.nix\";"
    ));
}

#[test]
fn rendering_refuses_a_format_it_does_not_write() {
    let list = StateManifest {
        version: 7,
        packages: names(&["git"]),
    };

    assert!(matches!(
        render(&user(), &list),
        Err(Unrenderable::State(Invalid::Newer(7)))
    ));
}

#[test]
fn rendering_refuses_a_list_without_git() {
    assert!(matches!(
        render(&user(), &manifest(&[])),
        Err(Unrenderable::State(Invalid::Missing("git")))
    ));
}

#[test]
fn rendering_rejects_an_invalid_package_name() {
    assert!(matches!(
        render(&user(), &manifest(&["git", "not a valid ident"])),
        Err(Unrenderable::Package(_))
    ));
}

#[test]
fn a_subject_names_a_short_list_of_packages() {
    assert_eq!(subject(&names(&["git", "fd"])), "git, fd");
}

#[test]
fn a_subject_counts_a_long_list_of_packages() {
    let packages: Vec<String> = (0..12).map(|i| format!("package-{i}")).collect();

    assert_eq!(subject(&packages), "12 packages");
}

fn config() -> UserConfig {
    UserConfig {
        user: user(),
        flake: "flake".into(),
        lock: "lock".into(),
        home: render_home(&user(), StateManifest::seed().packages).unwrap(),
        restored_state: None,
    }
}

fn drive(world: &mut World, mut runner: Runner, fail: impl Fn(&Action) -> bool) -> Report {
    make_guard!(guard);
    let mut runner = runner.brand(guard);
    let mut tree = Tree::new(
        Arc::new(Outbox::new("request", || {})),
        Arc::new(|| None),
        Start::command("install", Command::default()),
    );
    let mut input = None;
    loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(Ok(queries
                    .iter()
                    .map(|query| world.observe(query))
                    .collect())));
            }
            Next::Perform(action) if fail(&action) => {
                input = Some(Input::Done(Err(Failure::CommandFailed {
                    program: "nix build".into(),
                    status: Some(1),
                    output_tail: "error: attribute missing".into(),
                })));
            }
            Next::Perform(action) => input = Some(Input::Done(world.apply(&action))),
            Next::Finished(closed) => return runner.report(closed).clone(),
        }
    }
}

fn bootstrapped() -> World {
    let mut world = World::default();
    world.with_file("/usr/local/bin/mix-daemon", b"mix-daemon", 0o755, (0, 0));
    let config = config();
    world.with_dir(&config.user.home, 0o700, (config.user.uid, config.user.gid));
    world.users.insert(
        config.user.name.clone(),
        crate::action::UserFacts {
            uid: config.user.uid,
            gid: config.user.gid,
            home: config.user.home.clone(),
            shell: "/bin/sh".into(),
            comment: String::new(),
        },
    );
    let settings = Settings {
        policy: Policy::new(None, None).unwrap(),
        user: Some(config),
        force: false,
        runtime: Runtime {
            url: "https://mirror.internal/nix.tar.xz".into(),
            sha256: Digest([7; 32]),
            size: 1,
        },
        request: "bootstrap".into(),
        daemon: "/usr/local/bin/mix-daemon".into(),
    };
    let report = drive(
        &mut world,
        Runner::new(ROOT, crate::bootstrap::steps(&settings)),
        |_| false,
    );
    assert_eq!(report.verdict, Verdict::Succeeded);
    world
}

fn state_path() -> PathBuf {
    mix_state_dir(&user().home).join(STATE_FILE)
}

fn home_nix_path() -> PathBuf {
    mix_state_dir(&user().home).join(HOME_NIX)
}

fn settled_in(world: &World) -> Settled {
    settle(
        world
            .contents(state_path())
            .map(|raw| std::str::from_utf8(raw).unwrap()),
        None,
    )
}

fn active(world: &World) -> Option<u64> {
    world.profile(&user()).and_then(|profile| profile.active)
}

#[test]
fn an_install_writes_the_list_activates_it_and_records_it() {
    let mut world = bootstrapped();
    let before = active(&world);
    let requested = names(&["ripgrep"]);
    let change = install(&requested, settled_in(&world)).unwrap();

    let report = drive(
        &mut world,
        Runner::new(
            ROOT,
            steps(&user(), &change, mix_events::v1::Verb::Installing).unwrap(),
        ),
        |_| false,
    );

    assert_eq!(report.verdict, Verdict::Succeeded);
    let rendered = render(&user(), &manifest(&["git", "ripgrep"])).unwrap();
    assert_eq!(
        world.contents(state_path()),
        Some(rendered.state.as_bytes())
    );
    assert_eq!(
        world.contents(home_nix_path()),
        Some(rendered.home_nix.as_bytes())
    );
    assert_ne!(active(&world), before);
    assert!(matches!(
        world.observe(&Query::Path(mix_state_dir(&user().home).join(".git"))),
        Fact::Path(facts) if facts.kind == crate::action::Kind::Directory
    ));
}

#[test]
fn a_failed_activation_puts_both_files_back() {
    let mut world = bootstrapped();
    let state_before = world.contents(state_path()).map(<[u8]>::to_vec);
    let home_before = world.contents(home_nix_path()).map(<[u8]>::to_vec);
    let active_before = active(&world);
    let requested = names(&["doesnotexistinnixpkgs"]);
    let change = install(&requested, settled_in(&world)).unwrap();

    let report = drive(
        &mut world,
        Runner::new(
            ROOT,
            steps(&user(), &change, mix_events::v1::Verb::Installing).unwrap(),
        ),
        |action| matches!(action, Action::ActivateProfile { .. }),
    );

    assert!(matches!(report.verdict, Verdict::Failed { .. }));
    assert!(report.rollback_failures.is_empty());
    assert_eq!(
        world.contents(state_path()).map(<[u8]>::to_vec),
        state_before
    );
    assert_eq!(
        world.contents(home_nix_path()).map(<[u8]>::to_vec),
        home_before
    );
    assert_eq!(active(&world), active_before);
}

#[test]
fn a_list_restored_from_the_profile_is_written_without_activating() {
    let mut world = bootstrapped();
    world.with_file(state_path(), b"{broken", 0o644, (1000, 1000));
    let active_before = active(&world);
    let generation = manifest(&["git"]).render();
    let requested = names(&["git"]);
    let change = install(&requested, settle(Some("{broken"), Some(&generation))).unwrap();
    let steps = steps(&user(), &change, mix_events::v1::Verb::Installing).unwrap();

    assert_eq!(steps.len(), 1);
    let report = drive(&mut world, Runner::new(ROOT, steps), |_| false);

    assert_eq!(report.verdict, Verdict::Succeeded);
    assert_eq!(
        world.contents(state_path()),
        Some(manifest(&["git"]).render().as_bytes())
    );
    assert_eq!(active(&world), active_before);
}

#[test]
fn nothing_to_change_makes_no_plan() {
    let requested = names(&["git"]);
    let change = install(&requested, current(manifest(&["git"]), Source::File)).unwrap();

    assert!(
        steps(&user(), &change, mix_events::v1::Verb::Installing)
            .unwrap()
            .is_empty()
    );
}

fn settled_with_profile(world: &World) -> Settled {
    settle(
        world
            .contents(state_path())
            .map(|raw| std::str::from_utf8(raw).unwrap()),
        world
            .active_list(&user())
            .map(|raw| std::str::from_utf8(raw).unwrap()),
    )
}

type Decide = fn(&[String], Settled) -> Change;

fn installing(requested: &[String], settled: Settled) -> Change {
    install(requested, settled).unwrap()
}

fn removing(requested: &[String], settled: Settled) -> Change {
    remove(requested, settled).unwrap()
}

fn command(world: &mut World, decide: Decide, requested: &[String]) {
    let change = decide(requested, settled_with_profile(world));
    let steps = steps(&user(), &change, mix_events::v1::Verb::Installing).unwrap();
    let report = drive(world, Runner::new(ROOT, steps), |_| false);
    assert_eq!(report.verdict, Verdict::Succeeded);
}

fn crashing_command(
    world: &mut World,
    decide: Decide,
    requested: &[String],
    crash_at: usize,
    after_change: bool,
) -> Option<Vec<Record>> {
    let change = decide(requested, settled_with_profile(world));
    let mut runner = Runner::new(
        ROOT,
        steps(&user(), &change, mix_events::v1::Verb::Installing).unwrap(),
    );
    make_guard!(guard);
    let mut runner = runner.brand(guard);
    let mut tree = Tree::new(
        Arc::new(Outbox::new("request", || {})),
        Arc::new(|| None),
        Start::command("change", Command::default()),
    );
    let mut records = vec![Record::Began {
        request: "r".into(),
    }];
    let mut input = None;
    let mut seq = 0;
    loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(Ok(queries
                    .iter()
                    .map(|query| world.observe(query))
                    .collect())));
            }
            Next::Perform(action) => {
                if action == Action::Commit {
                    records.push(Record::Committing);
                }
                let undo = world
                    .clone()
                    .apply(&action)
                    .expect("the action applies")
                    .undo;
                records.push(Record::Prepared { seq, undo });
                if seq as usize == crash_at && !after_change {
                    return Some(records);
                }
                let outcome = world.apply(&action);
                if seq as usize == crash_at && after_change {
                    return Some(records);
                }
                records.push(Record::Done { seq });
                if action == Action::Commit {
                    records.push(Record::Ended);
                }
                seq += 1;
                input = Some(Input::Done(outcome));
            }
            Next::Finished(_) => return None,
        }
    }
}

fn recovered(world: &mut World, records: &[Record]) {
    match recover(records) {
        Recovery::Nothing => {}
        Recovery::RollBack { uncertain, certain } => {
            for action in uncertain {
                let _ = world.apply(&action);
            }
            for action in certain {
                world.apply(&action).expect("a certain undo applies");
            }
        }
        Recovery::FinishCommit { .. } => {
            world.apply(&Action::Commit).expect("the commit finishes");
        }
    }
}

fn listed(world: &World) -> Vec<String> {
    StateManifest::parse(std::str::from_utf8(world.contents(state_path()).unwrap()).unwrap())
        .unwrap()
        .packages
}

struct Scenario {
    name: &'static str,
    base: World,
    decide: Decide,
    requested: Vec<String>,
    wanted: Vec<String>,
}

fn scenarios() -> Vec<Scenario> {
    let installed = {
        let mut world = bootstrapped();
        command(&mut world, installing, &names(&["ripgrep", "fd"]));
        world
    };
    vec![
        Scenario {
            name: "install",
            base: bootstrapped(),
            decide: installing,
            requested: names(&["ripgrep", "fd"]),
            wanted: names(&["fd", "git", "ripgrep"]),
        },
        Scenario {
            name: "remove",
            base: installed,
            decide: removing,
            requested: names(&["ripgrep"]),
            wanted: names(&["fd", "git"]),
        },
    ]
}

#[test]
fn a_crash_at_any_action_of_a_change_is_settled_by_the_next_command() {
    for Scenario {
        name,
        base,
        decide,
        requested,
        wanted,
    } in scenarios()
    {
        let mut crash_at = 0;
        loop {
            let mut tried = false;
            for (after_change, recovering) in
                [(false, true), (true, true), (false, false), (true, false)]
            {
                let mut world = base.clone();
                let Some(records) =
                    crashing_command(&mut world, decide, &requested, crash_at, after_change)
                else {
                    continue;
                };
                tried = true;
                let at = format!(
                    "{name}: crash at action {crash_at}, after it: {after_change}, \
                     journal recovered: {recovering}"
                );

                if recovering {
                    recovered(&mut world, &records);
                }
                command(&mut world, decide, &requested);

                assert_eq!(
                    world.contents(state_path()),
                    world.active_list(&user()),
                    "{at}"
                );
                assert_eq!(listed(&world), wanted, "{at}");
            }
            if !tried {
                break;
            }
            crash_at += 1;
        }
        assert!(
            crash_at > 2,
            "{name} has too few actions to crash in: {crash_at}"
        );
    }
}

#[test]
fn a_finished_change_leaves_the_list_equal_to_the_profile() {
    for Scenario {
        name,
        base: mut world,
        decide,
        requested,
        wanted,
    } in scenarios()
    {
        command(&mut world, decide, &requested);

        assert_eq!(
            world.contents(state_path()),
            world.active_list(&user()),
            "{name}"
        );
        assert_eq!(listed(&world), wanted, "{name}");
    }
}

fn performed(
    world: &mut World,
    change: &Change,
    stop_before: Option<usize>,
) -> (Report, Vec<Action>) {
    let mut runner = Runner::new(
        ROOT,
        steps(&user(), change, mix_events::v1::Verb::Installing).unwrap(),
    );
    make_guard!(guard);
    let mut runner = runner.brand(guard);
    let mut tree = Tree::new(
        Arc::new(Outbox::new("request", || {})),
        Arc::new(|| None),
        Start::command("install", Command::default()),
    );
    let mut input = None;
    let mut actions = Vec::new();
    let mut forward = 0;
    loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(Ok(queries
                    .iter()
                    .map(|query| world.observe(query))
                    .collect())));
            }
            Next::Perform(action) => {
                if !runner.rolling_back() && action != Action::Commit {
                    if Some(forward) == stop_before {
                        runner.stop(Cancellation::Interrupted);
                    }
                    forward += 1;
                }
                actions.push(action.clone());
                input = Some(Input::Done(world.apply(&action)));
            }
            Next::Finished(closed) => return (runner.report(closed).clone(), actions),
        }
    }
}

#[test]
fn an_install_performs_exactly_the_writes_the_activation_the_record_and_the_commit() {
    let mut world = bootstrapped();
    let state_id = match world.observe(&Query::Path(state_path())) {
        Fact::Path(facts) => facts.id.unwrap(),
        other => panic!("the state file is observed as a path, not {other:?}"),
    };
    let home_id = match world.observe(&Query::Path(home_nix_path())) {
        Fact::Path(facts) => facts.id.unwrap(),
        other => panic!("home.nix is observed as a path, not {other:?}"),
    };
    let requested = names(&["ripgrep"]);
    let change = install(&requested, settled_in(&world)).unwrap();
    let rendered = render(&user(), &change.manifest).unwrap();

    let (report, actions) = performed(&mut world, &change, None);

    assert_eq!(report.verdict, Verdict::Succeeded);
    assert_eq!(
        actions,
        vec![
            Action::PutFile {
                path: state_path(),
                contents: Arc::from(rendered.state.as_bytes()),
                mode: 0o644,
                owner: Some((1000, 1000)),
                expect: Expect::Present(state_id),
            },
            Action::PutFile {
                path: home_nix_path(),
                contents: Arc::from(rendered.home_nix.as_bytes()),
                mode: 0o644,
                owner: Some((1000, 1000)),
                expect: Expect::Present(home_id),
            },
            Action::ActivateProfile {
                user: user(),
                source: crate::action::FlakeSource::Git,
            },
            Action::RecordState { user: user() },
            Action::Commit,
        ]
    );
}

#[test]
fn removing_a_package_that_is_not_installed_makes_no_plan() {
    let requested = names(&["ripgrep"]);
    let change = remove(&requested, current(manifest(&["git"]), Source::File)).unwrap();

    assert_eq!(change.skipped, names(&["ripgrep"]));
    assert!(
        steps(&user(), &change, mix_events::v1::Verb::Removing)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_install_over_a_broken_list_restores_it_and_adds_the_package() {
    let mut world = bootstrapped();
    let generation = manifest(&["git", "fd"]).render();
    world.with_file(state_path(), b"{broken", 0o644, (1000, 1000));
    let requested = names(&["ripgrep"]);
    let change = install(&requested, settle(Some("{broken"), Some(&generation))).unwrap();

    let (report, actions) = performed(&mut world, &change, None);

    assert_eq!(report.verdict, Verdict::Succeeded);
    assert_eq!(change.source, Source::Generation);
    assert_eq!(listed(&world), names(&["fd", "git", "ripgrep"]));
    assert!(actions.contains(&Action::ActivateProfile {
        user: user(),
        source: crate::action::FlakeSource::Git,
    }));
}

#[test]
fn an_invalid_package_name_is_refused_before_any_action() {
    let requested = names(&["not a valid ident"]);
    let change = install(&requested, current(manifest(&["git"]), Source::File)).unwrap();

    assert!(matches!(
        steps(&user(), &change, mix_events::v1::Verb::Installing),
        Err(Unrenderable::Package(_))
    ));
}

#[test]
fn an_interrupt_before_any_action_puts_the_change_back_unless_only_the_record_is_left() {
    let base = bootstrapped();
    let requested = names(&["ripgrep"]);
    let change = install(&requested, settled_in(&base)).unwrap();
    let mut finished = base.clone();
    let (_, actions) = performed(&mut finished, &change, None);
    let forward = actions
        .iter()
        .filter(|action| **action != Action::Commit)
        .count();

    for stop_before in 0..forward {
        let mut world = base.clone();

        let (report, actions) = performed(&mut world, &change, Some(stop_before));

        let record_left = actions
            .iter()
            .filter(|action| **action != Action::Commit)
            .position(|action| matches!(action, Action::RecordState { .. }))
            == Some(stop_before);
        if record_left {
            assert_eq!(
                report.verdict,
                Verdict::Succeeded,
                "stop before {stop_before}"
            );
            assert_eq!(listed(&world), names(&["git", "ripgrep"]));
        } else {
            assert!(
                matches!(report.verdict, Verdict::Cancelled(_)),
                "stop before {stop_before}: {:?}",
                report.verdict
            );
            assert!(
                report.rollback_failures.is_empty(),
                "stop before {stop_before}"
            );
            assert_eq!(world.contents(state_path()), base.contents(state_path()));
            assert_eq!(
                world.contents(home_nix_path()),
                base.contents(home_nix_path())
            );
            assert_eq!(active(&world), active(&base), "stop before {stop_before}");
        }
    }
}

#[test]
fn the_written_files_keep_their_kind_and_owner() {
    let mut world = bootstrapped();
    let requested = names(&["ripgrep"]);
    let change = install(&requested, settled_in(&world)).unwrap();

    performed(&mut world, &change, None);

    for path in [state_path(), home_nix_path()] {
        let Fact::Path(facts) = world.observe(&Query::Path(path.clone())) else {
            panic!("{} is observed as a path", path.display());
        };
        assert_eq!(facts.kind, Kind::File, "{}", path.display());
        assert_eq!(facts.owner, (1000, 1000), "{}", path.display());
        assert_eq!(facts.mode, 0o644, "{}", path.display());
    }
}

proptest::proptest! {
    #[test]
    fn anything_validate_accepts_renders_to_the_same_list(raw in ".*") {
        if let Ok(parsed) = validate(&raw) {
            proptest::prop_assert_eq!(validate(&parsed.render()), Ok(parsed));
        }
    }

    #[test]
    fn a_valid_list_survives_any_single_byte_change_as_valid_or_rejected(
        packages in proptest::collection::vec("[a-z][a-z0-9-]{0,12}", 0..6),
        position in proptest::prelude::any::<proptest::sample::Index>(),
        byte in proptest::prelude::any::<u8>(),
    ) {
        let mut list = vec!["git".to_string()];
        list.extend(packages);
        let rendered = StateManifest { version: STATE_VERSION, packages: list }.render();
        let mut bytes = rendered.into_bytes();
        let at = position.index(bytes.len());
        bytes[at] = byte;
        if let Ok(raw) = String::from_utf8(bytes)
            && let Ok(parsed) = validate(&raw)
        {
            proptest::prop_assert!(parsed.packages.iter().all(|p| mix_nixgen::is_identifier(p)));
            proptest::prop_assert!(parsed.packages.iter().any(|p| p == "git"));
        }
    }

    #[test]
    fn an_install_then_a_remove_of_the_same_packages_gives_the_list_back(
        installed in proptest::collection::vec("[a-z][a-z0-9-]{0,8}", 0..5),
        requested in proptest::collection::vec("[a-z][a-z0-9-]{0,8}", 1..5),
    ) {
        let mut before = vec!["git".to_string()];
        for package in installed {
            if !before.contains(&package) {
                before.push(package);
            }
        }
        let before = StateManifest { version: STATE_VERSION, packages: before }.sorted();
        let installed = install(&requested, current(before.clone(), Source::File)).unwrap();
        let added = installed.changed.clone();
        let removed = remove(&added, current(installed.manifest, Source::File)).unwrap();

        proptest::prop_assert_eq!(removed.changed, added);
        proptest::prop_assert_eq!(removed.manifest, before);
    }
}

#[test]
fn the_same_packages_requested_in_any_order_render_the_same_files() {
    let render_of = |requested: &[&str]| {
        let change = install(
            &names(requested),
            current(StateManifest::seed(), Source::File),
        )
        .unwrap();
        render(&user(), &change.manifest).unwrap()
    };

    assert_eq!(render_of(&["jq", "hello"]), render_of(&["hello", "jq"]));
}

#[test]
fn a_list_written_in_another_order_returns_to_the_generation_already_built() {
    let mut world = bootstrapped();
    command(&mut world, installing, &names(&["hello", "jq"]));
    let built = active(&world);
    command(&mut world, removing, &names(&["hello", "jq"]));

    command(&mut world, installing, &names(&["jq", "hello"]));

    assert_eq!(active(&world), built);
}

fn clean(world: &mut World, all: bool) -> Vec<Action> {
    let performed = std::cell::RefCell::new(Vec::new());
    let report = drive(
        world,
        Runner::new(ROOT, clean_steps(&user(), all)),
        |action| {
            performed.borrow_mut().push(action.clone());
            false
        },
    );
    assert_eq!(report.verdict, Verdict::Succeeded);
    performed.into_inner()
}

#[test]
fn changes_keep_every_generation_until_a_clean() {
    let mut world = bootstrapped();
    for package in ["fd", "jq", "bat"] {
        command(&mut world, installing, &names(&[package]));
    }
    let before = world.profile(&user()).unwrap().generations.len();
    assert!(before >= 4);

    let performed = clean(&mut world, false);

    let profile = world.profile(&user()).unwrap();
    assert_eq!(
        profile.generations,
        profile.active.into_iter().collect::<Vec<_>>()
    );
    assert_eq!(listed(&world), names(&["bat", "fd", "git", "jq"]));
    assert!(
        !performed
            .iter()
            .any(|action| matches!(action, Action::CollectGarbage { .. }))
    );
}

#[test]
fn a_clean_with_all_also_collects_the_store() {
    let mut world = bootstrapped();

    let performed = clean(&mut world, true);

    assert!(performed.contains(&Action::CollectGarbage { user: user() }));
}

#[test]
fn old_generations_are_all_but_the_active_one() {
    let profile = ProfileFacts {
        generations: vec![1, 2, 3, 4],
        active: Some(3),
    };

    assert_eq!(old_generations(&profile), vec![1, 2, 4]);
    assert_eq!(old_generations(&ProfileFacts::default()), Vec::<u64>::new());
}
