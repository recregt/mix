use std::path::PathBuf;

use mix_events::ROOT;
use mix_events::v1::command::Request;
use mix_events::v1::{CleanRequest, InstallRequest, RemoveRequest};

use super::*;
use crate::declared::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
use crate::declared::policy::Policy;
use crate::declared::targets::Runtime;
use crate::declared::targets::UserConfig;
use crate::effect::{Action, Fact, Failure, Query};
use crate::effect::{Digest, Expect};
use crate::model::World;
use crate::model::testkit::{Run, Script, drive, recover, requested};
use crate::ops::bootstrap::Settings;
use crate::run::{Runner, Verdict};

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

fn install_request(packages: &[String]) -> Request {
    Request::Install(InstallRequest {
        packages: packages.to_vec(),
    })
}

fn remove_request(packages: &[String]) -> Request {
    Request::Remove(RemoveRequest {
        packages: packages.to_vec(),
    })
}

fn run(world: &mut World, request: Request, change: &Change, script: &Script) -> Run {
    let steps = steps(
        &config(),
        &Policy::new(None, None).unwrap(),
        change,
        mix_events::v1::Verb::Installing,
        "request",
    )
    .unwrap();
    drive(world, Runner::new(ROOT, steps), requested(request), script)
}

fn bootstrapped() -> World {
    let mut world = World::default();
    world.with_file(
        crate::declared::paths::RUNNING_PROGRAM,
        b"mix-daemon",
        0o755,
        (0, 0),
    );
    let config = config();
    world.with_dir(&config.user.home, 0o700, (config.user.uid, config.user.gid));
    world.users.insert(
        config.user.name.clone(),
        crate::effect::UserFacts {
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
    };
    let run = drive(
        &mut world,
        Runner::new(ROOT, crate::ops::bootstrap::steps(&settings)),
        requested(Request::Bootstrap(Box::default())),
        &Script::default(),
    );
    assert_eq!(run.report().verdict, Verdict::Succeeded);
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

fn id_of(world: &World, path: PathBuf) -> crate::effect::FileId {
    match world.observe(&Query::Path(path)) {
        Fact::Path(facts) => facts.id.unwrap(),
        other => panic!("a file is observed as a path, not {other:?}"),
    }
}

#[test]
fn an_install_writes_the_list_activates_it_and_records_it() {
    let mut world = bootstrapped();
    let expected = vec![
        Expect::Present(id_of(&world, home_nix_path())),
        Expect::Present(id_of(&world, state_path())),
    ];
    let requested = names(&["ripgrep"]);
    let change = install(&requested, settled_in(&world)).unwrap();

    let run = run(
        &mut world,
        install_request(&requested),
        &change,
        &Script::default(),
    );

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert_eq!(
        run.performed(),
        [
            format!("PutFile {}", home_nix_path().display()),
            format!("PutFile {}", state_path().display()),
            "ActivateProfile mix-user's profile".to_string(),
            "RecordState mix-user's package list".to_string(),
            "Commit changes".to_string(),
        ],
        "the list is written, then activated, then recorded"
    );
    assert_eq!(
        world.contents(state_path()),
        Some(manifest(&["git", "ripgrep"]).render().as_bytes())
    );
    let home = String::from_utf8(world.contents(home_nix_path()).unwrap().to_vec()).unwrap();
    assert!(home.contains("pkgs.ripgrep"), "{home}");
    assert_eq!(active(&world), Some(2));
    let expects: Vec<Expect> = run
        .performed
        .iter()
        .filter_map(|action| match action {
            Action::PutFile { expect, .. } => Some(*expect),
            _ => None,
        })
        .collect();
    assert_eq!(expects, expected, "a write replaces only the file it read");
}

#[test]
fn a_failed_activation_puts_both_files_back() {
    let mut world = bootstrapped();
    let before = world.clone();
    let requested = names(&["doesnotexistinnixpkgs"]);
    let change = install(&requested, settled_in(&world)).unwrap();
    let script = Script::failing_when(
        |action| matches!(action, Action::ActivateProfile { .. }),
        Failure::CommandFailed {
            program: "nix build".into(),
            status: Some(1),
            output_tail: "error: attribute missing".into(),
        },
    );

    let run = run(&mut world, install_request(&requested), &change, &script);

    assert!(matches!(run.report().verdict, Verdict::Failed { .. }));
    assert_eq!(
        run.performed()[3..],
        [
            format!("Restore {}", state_path().display()),
            format!("Restore {}", home_nix_path().display()),
        ],
        "the files go back in the reverse of the order they were written"
    );
    for path in [state_path(), home_nix_path()] {
        assert_eq!(
            world.contents(&path),
            before.contents(&path),
            "{}",
            path.display()
        );
    }
    assert_eq!(active(&world), active(&before));
}

#[test]
fn a_list_restored_from_the_profile_is_written_without_activating() {
    let mut world = bootstrapped();
    world.with_file(state_path(), b"{broken", 0o644, (1000, 1000));
    let before = world.clone();
    let generation = manifest(&["git"]).render();
    let requested = names(&["git"]);
    let change = install(&requested, settle(Some("{broken"), Some(&generation))).unwrap();

    let run = run(
        &mut world,
        install_request(&requested),
        &change,
        &Script::default(),
    );

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert_eq!(
        run.performed(),
        [
            format!("PutFile {}", state_path().display()),
            "Commit changes".to_string()
        ]
    );
    assert_eq!(world.contents(state_path()), Some(generation.as_bytes()));
    assert_eq!(active(&world), active(&before), "nothing was activated");
}

#[test]
fn nothing_to_change_makes_no_plan() {
    let requested = names(&["git"]);
    let change = install(&requested, current(manifest(&["git"]), Source::File)).unwrap();

    assert!(
        steps(
            &config(),
            &Policy::new(None, None).unwrap(),
            &change,
            mix_events::v1::Verb::Installing,
            "request"
        )
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

#[derive(Clone, Copy)]
enum Verb {
    Install,
    Remove,
}

fn decided(verb: Verb, requested: &[String], world: &World) -> (Request, Change) {
    let settled = settled_with_profile(world);
    match verb {
        Verb::Install => (
            install_request(requested),
            install(requested, settled).unwrap(),
        ),
        Verb::Remove => (
            remove_request(requested),
            remove(requested, settled).unwrap(),
        ),
    }
}

fn command(world: &mut World, verb: Verb, requested: &[String]) {
    let (request, change) = decided(verb, requested, world);
    let run = run(world, request, &change, &Script::default());
    assert_eq!(run.report().verdict, Verdict::Succeeded);
}

fn listed(world: &World) -> Vec<String> {
    StateManifest::parse(std::str::from_utf8(world.contents(state_path()).unwrap()).unwrap())
        .unwrap()
        .packages
}

struct Scenario {
    name: &'static str,
    base: World,
    verb: Verb,
    requested: Vec<String>,
    wanted: Vec<String>,
}

fn scenarios() -> Vec<Scenario> {
    let installed = {
        let mut world = bootstrapped();
        command(&mut world, Verb::Install, &names(&["ripgrep", "fd"]));
        world
    };
    vec![
        Scenario {
            name: "install",
            base: bootstrapped(),
            verb: Verb::Install,
            requested: names(&["ripgrep", "fd"]),
            wanted: names(&["fd", "git", "ripgrep"]),
        },
        Scenario {
            name: "remove",
            base: installed,
            verb: Verb::Remove,
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
        verb,
        requested,
        wanted,
    } in scenarios()
    {
        let mut crash_at = 0;
        loop {
            let mut tried = false;
            for (after, recovering) in [(false, true), (true, true), (false, false), (true, false)]
            {
                let mut world = base.clone();
                let (request, change) = decided(verb, &requested, &world);
                let crashed = run(
                    &mut world,
                    request,
                    &change,
                    &Script::crashing(crash_at, after),
                );
                if !crashed.crashed {
                    continue;
                }
                tried = true;
                let at = format!(
                    "{name}: crash at action {crash_at}, after it: {after}, \
                     journal recovered: {recovering}"
                );

                if recovering {
                    recover(&mut world, &crashed.journal);
                }
                command(&mut world, verb, &requested);

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
        verb,
        requested,
        wanted,
    } in scenarios()
    {
        command(&mut world, verb, &requested);

        assert_eq!(
            world.contents(state_path()),
            world.active_list(&user()),
            "{name}"
        );
        assert_eq!(listed(&world), wanted, "{name}");
    }
}

#[test]
fn removing_a_package_that_is_not_installed_makes_no_plan() {
    let requested = names(&["ripgrep"]);
    let change = remove(&requested, current(manifest(&["git"]), Source::File)).unwrap();

    assert_eq!(change.skipped, names(&["ripgrep"]));
    assert!(
        steps(
            &config(),
            &Policy::new(None, None).unwrap(),
            &change,
            mix_events::v1::Verb::Removing,
            "request"
        )
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

    let run = run(
        &mut world,
        install_request(&requested),
        &change,
        &Script::default(),
    );

    assert_eq!(change.source, Source::Generation);
    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert_eq!(
        world.contents(state_path()),
        Some(manifest(&["fd", "git", "ripgrep"]).render().as_bytes()),
        "the list the profile remembers, plus the new package"
    );
    let home = String::from_utf8(world.contents(home_nix_path()).unwrap().to_vec()).unwrap();
    assert!(
        ["pkgs.fd", "pkgs.git", "pkgs.ripgrep"]
            .iter()
            .all(|name| home.contains(name)),
        "{home}"
    );
}

#[test]
fn a_remove_over_a_broken_list_restores_it_and_drops_the_package() {
    let mut world = bootstrapped();
    let generation = manifest(&["git", "fd"]).render();
    world.with_file(state_path(), b"{broken", 0o644, (1000, 1000));
    let requested = names(&["fd"]);
    let change = remove(&requested, settle(Some("{broken"), Some(&generation))).unwrap();

    let run = run(
        &mut world,
        remove_request(&requested),
        &change,
        &Script::default(),
    );

    assert_eq!(change.source, Source::Generation);
    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert_eq!(
        world.contents(state_path()),
        Some(manifest(&["git"]).render().as_bytes()),
        "the list the profile remembers, without the removed package"
    );
}

#[test]
fn an_invalid_package_name_is_refused_before_any_action() {
    let requested = names(&["not a valid ident"]);
    let change = install(&requested, current(manifest(&["git"]), Source::File)).unwrap();

    assert!(matches!(
        steps(
            &config(),
            &Policy::new(None, None).unwrap(),
            &change,
            mix_events::v1::Verb::Installing,
            "request"
        ),
        Err(Unrenderable::Package(_))
    ));
}

#[test]
fn an_interrupt_at_any_change_puts_the_change_back_unless_only_the_record_is_left() {
    let base = bootstrapped();
    let requested = names(&["ripgrep"]);
    let change = install(&requested, settled_in(&base)).unwrap();
    let finished = run(
        &mut base.clone(),
        install_request(&requested),
        &change,
        &Script::default(),
    );

    for stop in 0..finished.changes {
        let mut world = base.clone();

        let stopped = run(
            &mut world,
            install_request(&requested),
            &change,
            &Script::stopping_after(stop),
        );

        let record_left = finished
            .performed
            .iter()
            .filter(|action| **action != Action::Commit)
            .position(|action| matches!(action, Action::RecordState { .. }))
            == Some(stop);
        let report = stopped.report();
        if record_left {
            assert_eq!(report.verdict, Verdict::Succeeded, "stop at {stop}");
            assert_eq!(listed(&world), names(&["git", "ripgrep"]));
        } else {
            assert!(
                matches!(report.verdict, Verdict::Cancelled(_)),
                "stop at {stop}: {:?}",
                report.verdict
            );
            assert!(report.rollback_failures.is_empty(), "stop at {stop}");
            assert_eq!(world.contents(state_path()), base.contents(state_path()));
            assert_eq!(
                world.contents(home_nix_path()),
                base.contents(home_nix_path())
            );
            assert_eq!(active(&world), active(&base), "stop at {stop}");
        }
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
    command(&mut world, Verb::Install, &names(&["hello", "jq"]));
    let built = active(&world);
    command(&mut world, Verb::Remove, &names(&["hello", "jq"]));

    command(&mut world, Verb::Install, &names(&["jq", "hello"]));

    assert_eq!(active(&world), built);
}

fn clean(world: &mut World, all: bool) -> Run {
    let current = UserConfig {
        home: render_home(&user(), listed(world)).unwrap(),
        ..config()
    };
    drive(
        world,
        Runner::new(
            ROOT,
            clean_steps(&current, &Policy::new(None, None).unwrap(), all, "request"),
        ),
        requested(Request::Clean(CleanRequest { all })),
        &Script::default(),
    )
}

#[test]
fn changes_keep_every_generation_until_a_clean() {
    let mut world = bootstrapped();
    for package in ["fd", "jq", "bat"] {
        command(&mut world, Verb::Install, &names(&[package]));
    }

    let run = clean(&mut world, false);

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    let profile = world.profile(&user()).unwrap();
    assert_eq!(profile.generations, [4]);
    assert_eq!(profile.active, Some(4), "the active generation is kept");
}

#[test]
fn a_clean_with_nothing_old_changes_nothing() {
    let mut world = bootstrapped();
    let before = world.clone();

    let run = clean(&mut world, false);

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert_eq!(run.changes, 0);
    assert_eq!(world, before);
}

#[test]
fn a_clean_with_all_also_collects_the_store() {
    let mut world = bootstrapped();

    let run = clean(&mut world, true);

    assert_eq!(run.report().verdict, Verdict::Succeeded);
    assert_eq!(
        run.performed(),
        ["CollectGarbage unused store paths", "Commit changes"]
    );
}

#[test]
fn old_generations_are_all_but_the_active_one() {
    let profile = ProfileFacts {
        generations: vec![1, 2, 3, 4],
        active: Some(3),
        ..ProfileFacts::default()
    };

    assert_eq!(old_generations(&profile), vec![1, 2, 4]);
    assert_eq!(old_generations(&ProfileFacts::default()), Vec::<u64>::new());
}
