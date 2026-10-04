use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_events::v1::Command;
use mix_events::{Outbox, ROOT, Start, Tree};

use super::*;
use crate::action::Digest;
use crate::bootstrap::{Runtime, Settings, steps};
use crate::identity::InvokingUser;
use crate::paths::{
    DEFAULT_PROFILE_NIX_ENV, INDEX_LOCK, NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_UNIT,
    POLICY_FILE, STATE_FILE, mix_state_dir, repository_dir,
};
use crate::plan::{Input, Next, Report, Runner, StepOutcome, make_guard};
use crate::policy::Policy;
use crate::targets::{UserConfig, targets};
use crate::world::World;

/// The binding between what an inspection found and what repair can do about it.
#[test]
fn a_finding_says_whether_repair_can_reconcile_it() {
    assert_eq!(
        Finding::NotADirectory.unfixable(),
        Some(Unfixable::NotADirectory)
    );
    assert_eq!(
        Finding::NoSuchUser.unfixable(),
        Some(Unfixable::MissingUser)
    );
    assert_eq!(
        Finding::RuntimeMissing.unfixable(),
        Some(Unfixable::MissingRuntime)
    );
}

#[test]
fn everything_repair_reconciles_is_bound_to_no_reason() {
    for finding in [
        Finding::Missing,
        Finding::Unreadable {
            kind: std::io::ErrorKind::PermissionDenied,
        },
        Finding::Mode {
            actual: 0o700,
            expected: 0o755,
        },
        Finding::Owner {
            actual: (0, 0),
            expected: (1000, 1000),
        },
        Finding::ContentDrift,
        Finding::GroupMissing,
        Finding::GroupGid {
            actual: 1,
            expected: 30_000,
        },
        Finding::NotAMember { group: "mix-users" },
        Finding::UserMissing,
        Finding::UserIds {
            actual: (1, 1),
            expected: (30_000, 30_000),
        },
        Finding::UnitMissing,
        Finding::UnitDrift,
        Finding::UnitInactive,
        Finding::RepositoryBroken,
        Finding::RepositoryLocked,
    ] {
        assert_eq!(finding.unfixable(), None, "{finding:?}");
    }
}

fn policy() -> Policy {
    Policy::new(Some("https://mirror.internal"), Some("mirror:AAAA")).unwrap()
}

fn user(name: &str, uid: u32) -> UserConfig {
    UserConfig {
        user: InvokingUser {
            uid,
            gid: uid,
            name: name.into(),
            home: PathBuf::from("/home").join(name),
        },
        flake: format!("flake of {name}"),
        lock: "lock".into(),
        home: format!("home of {name}"),
        restored_state: None,
    }
}

fn drive(world: &mut World, mut runner: Runner) -> Report {
    make_guard!(guard);
    let mut runner = runner.brand(guard);
    let outbox = Arc::new(Outbox::new("request", || {}));
    let mut tree = Tree::new(
        outbox,
        Arc::new(|| None),
        Start::command("health", Command::default()),
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
            Next::Perform(action) => input = Some(Input::Done(world.apply(&action))),
            Next::Finished(closed) => return runner.report(closed).clone(),
        }
    }
}

fn bootstrapped(users: &[UserConfig]) -> World {
    let mut world = World::default();
    world.with_file("/usr/local/bin/mix-daemon", b"mix-daemon", 0o755, (0, 0));
    world.with_file(crate::paths::RUNNING_PROGRAM, b"mix-daemon", 0o755, (0, 0));
    for config in users {
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
            policy: policy(),
            user: Some(config.clone()),
            force: false,
            runtime: Runtime {
                url: "https://mirror.internal/nix.tar.xz".into(),
                sha256: Digest([7; 32]),
                size: 1,
            },
            request: "bootstrap".into(),
            daemon: "/usr/local/bin/mix-daemon".into(),
        };
        let report = drive(&mut world, Runner::new(ROOT, steps(&settings)));
        assert_eq!(report.verdict, crate::plan::Verdict::Succeeded);
    }
    world
}

fn audit(world: &World, config: &UserConfig) -> Vec<(String, Finding)> {
    let policy = policy();
    targets(Some(config), &policy)
        .iter()
        .filter_map(|target| {
            let facts: Vec<Fact> = queries(target)
                .iter()
                .map(|query| world.observe(query))
                .collect();
            classify(target, &facts).map(|finding| (target.label().into_owned(), finding))
        })
        .collect()
}

fn repair(world: &mut World, config: &UserConfig) -> Report {
    let policy = policy();
    drive(
        world,
        Runner::new(ROOT, repair_steps(targets(Some(config), &policy), "repair")).independent(),
    )
}

type Drift = fn(&mut World);

const ALICE_REPOSITORY: &str = "/home/alice/.local/state/mix/.git";
const ALICE_GENERATIONS: &str = "/home/alice/.local/state/nix/profiles/home-manager";

fn remove_tree(world: &mut World, top: &Path) {
    world.files.retain(|path, _| !path.starts_with(top));
}

type Found = Vec<(&'static str, Finding)>;

fn drifts() -> Vec<(&'static str, Drift, Found)> {
    vec![
        (
            "a missing directory",
            |world: &mut World| {
                world.files.remove(Path::new("/nix/var/nix/temproots"));
            },
            vec![("/nix/var/nix/temproots", Finding::Missing)],
        ),
        (
            "a file where a directory belongs",
            |world: &mut World| {
                world.files.remove(Path::new("/nix/var/nix/temproots"));
                world.with_file("/nix/var/nix/temproots", b"x", 0o644, (0, 0));
            },
            vec![("/nix/var/nix/temproots", Finding::NotADirectory)],
        ),
        (
            "a directory mode",
            |world: &mut World| {
                world
                    .files
                    .get_mut(Path::new("/nix/var/nix/temproots"))
                    .unwrap()
                    .mode = 0o700;
            },
            vec![(
                "/nix/var/nix/temproots",
                Finding::Mode {
                    actual: 0o700,
                    expected: 0o755,
                },
            )],
        ),
        (
            "a user's directory taken by root",
            |world: &mut World| {
                let state = mix_state_dir(Path::new("/home/alice"));
                world.files.get_mut(&state).unwrap().owner = (0, 0);
            },
            vec![(
                "/home/alice/.local/state/mix",
                Finding::Owner {
                    actual: (0, 0),
                    expected: (1000, 1000),
                },
            )],
        ),
        (
            "a rewritten configuration file",
            |world: &mut World| {
                world.with_file(NIX_CONF_DEST, b"trusted-users = *\n", 0o644, (0, 0));
            },
            vec![(NIX_CONF_DEST, Finding::ContentDrift)],
        ),
        (
            "a missing configuration file",
            |world: &mut World| {
                world.files.remove(Path::new(POLICY_FILE));
            },
            vec![(POLICY_FILE, Finding::Missing)],
        ),
        (
            "a missing seeded file",
            |world: &mut World| {
                let state = mix_state_dir(Path::new("/home/alice"));
                world.files.remove(&state.join(STATE_FILE));
            },
            vec![("/home/alice/.local/state/mix/state", Finding::Missing)],
        ),
        (
            "a missing group",
            |world: &mut World| {
                world.groups.remove("mix-users");
            },
            vec![
                ("mix-users", Finding::GroupMissing),
                ("alice", Finding::NotAMember { group: "mix-users" }),
            ],
        ),
        (
            "a group with another gid",
            |world: &mut World| {
                world.groups.get_mut("nixbld").unwrap().gid = 31_000;
            },
            vec![(
                "nixbld",
                Finding::GroupGid {
                    actual: 31_000,
                    expected: 30_000,
                },
            )],
        ),
        (
            "a user taken out of the group",
            |world: &mut World| {
                world
                    .groups
                    .get_mut("mix-users")
                    .unwrap()
                    .members
                    .retain(|member| member != "alice");
            },
            vec![("alice", Finding::NotAMember { group: "mix-users" })],
        ),
        (
            "an enrolled user whose account is gone",
            |world: &mut World| {
                world.users.remove("alice");
                for group in world.groups.values_mut() {
                    group.members.retain(|member| member != "alice");
                }
            },
            vec![("alice", Finding::NoSuchUser)],
        ),
        (
            "a user's file taken by root",
            |world: &mut World| {
                let state = mix_state_dir(Path::new("/home/alice"));
                world.files.get_mut(&state.join(STATE_FILE)).unwrap().owner = (0, 0);
            },
            vec![(
                "/home/alice/.local/state/mix/state",
                Finding::Owner {
                    actual: (0, 0),
                    expected: (1000, 1000),
                },
            )],
        ),
        (
            "a missing build user",
            |world: &mut World| {
                world.users.remove("nixbld3");
                for group in world.groups.values_mut() {
                    group.members.retain(|member| member != "nixbld3");
                }
            },
            vec![("nixbld3", Finding::UserMissing)],
        ),
        (
            "a build user with other ids",
            |world: &mut World| {
                world.users.get_mut("nixbld3").unwrap().uid = 40_003;
            },
            vec![(
                "nixbld3",
                Finding::UserIds {
                    actual: (40_003, 30_000),
                    expected: (30_003, 30_000),
                },
            )],
        ),
        (
            "a missing unit",
            |world: &mut World| {
                world.files.remove(Path::new(NIX_DAEMON_SOCKET_DEST));
            },
            vec![(NIX_DAEMON_SOCKET_UNIT, Finding::UnitMissing)],
        ),
        (
            "a unit whose source cannot be read",
            |world: &mut World| {
                world
                    .files
                    .remove(Path::new(crate::paths::NIX_DAEMON_SOCKET_SRC));
            },
            vec![],
        ),
        (
            "a changed unit",
            |world: &mut World| {
                world.with_file(NIX_DAEMON_SOCKET_DEST, b"[Socket]\n", 0o644, (0, 0));
            },
            vec![(NIX_DAEMON_SOCKET_UNIT, Finding::UnitDrift)],
        ),
        (
            "a stopped socket",
            |world: &mut World| {
                world
                    .apply(&Action::StopUnit {
                        unit: NIX_DAEMON_SOCKET_UNIT.to_string(),
                    })
                    .unwrap();
            },
            vec![(NIX_DAEMON_SOCKET_UNIT, Finding::UnitInactive)],
        ),
        (
            "a state directory opened to others",
            |world: &mut World| {
                let state = mix_state_dir(Path::new("/home/alice"));
                world.files.get_mut(&state).unwrap().mode = 0o755;
            },
            vec![(
                "/home/alice/.local/state/mix",
                Finding::Mode {
                    actual: 0o755,
                    expected: 0o700,
                },
            )],
        ),
        (
            "a wiped state directory",
            |world: &mut World| {
                remove_tree(world, &mix_state_dir(Path::new("/home/alice")));
            },
            vec![
                ("/home/alice/.local/state/mix", Finding::Missing),
                ("/home/alice/.local/state/mix/home.nix", Finding::Missing),
                ("/home/alice/.local/state/mix/flake.nix", Finding::Missing),
                ("/home/alice/.local/state/mix/flake.lock", Finding::Missing),
                ("/home/alice/.local/state/mix/state", Finding::Missing),
                (ALICE_REPOSITORY, Finding::Missing),
            ],
        ),
        (
            "a missing repository",
            |world: &mut World| {
                remove_tree(world, &repository_dir(Path::new("/home/alice")));
            },
            vec![(ALICE_REPOSITORY, Finding::Missing)],
        ),
        (
            "a repository whose history does not verify",
            |world: &mut World| {
                let head =
                    repository_dir(Path::new("/home/alice")).join(crate::world::REPOSITORY_HEAD);
                world.files.remove(&head);
            },
            vec![(ALICE_REPOSITORY, Finding::RepositoryBroken)],
        ),
        (
            "a file where the repository belongs",
            |world: &mut World| {
                let repository = repository_dir(Path::new("/home/alice"));
                remove_tree(world, &repository);
                world.with_file(&repository, b"gitdir: /elsewhere\n", 0o644, (1000, 1000));
            },
            vec![(ALICE_REPOSITORY, Finding::RepositoryBroken)],
        ),
        (
            "an index lock no mix command holds",
            |world: &mut World| {
                let lock = repository_dir(Path::new("/home/alice")).join(INDEX_LOCK);
                world.with_file(lock, b"", 0o644, (1000, 1000));
            },
            vec![(ALICE_REPOSITORY, Finding::RepositoryLocked)],
        ),
        (
            "an altered daemon binary",
            |world: &mut World| {
                world.with_file(crate::paths::MIX_DAEMON_BIN, b"tampered", 0o755, (0, 0));
            },
            vec![(crate::paths::MIX_DAEMON_BIN, Finding::ContentDrift)],
        ),
        (
            "a missing daemon binary",
            |world: &mut World| {
                world.files.remove(Path::new(crate::paths::MIX_DAEMON_BIN));
            },
            vec![(crate::paths::MIX_DAEMON_BIN, Finding::Missing)],
        ),
        (
            "a daemon binary anyone can write",
            |world: &mut World| {
                world
                    .files
                    .get_mut(Path::new(crate::paths::MIX_DAEMON_BIN))
                    .unwrap()
                    .mode = 0o777;
            },
            vec![(
                crate::paths::MIX_DAEMON_BIN,
                Finding::Mode {
                    actual: 0o777,
                    expected: 0o755,
                },
            )],
        ),
        (
            "an interrupted request recovery could not put back",
            |world: &mut World| {
                world.journals = vec!["r9".into()];
            },
            vec![(
                crate::paths::JOURNAL_DIR,
                Finding::Interrupted {
                    requests: vec!["r9".into()],
                },
            )],
        ),
        (
            "a backup an interrupted write left",
            |world: &mut World| {
                let state = mix_state_dir(Path::new("/home/alice"));
                world.with_file(
                    state.join(".state.mix-backup-r9-1"),
                    b"{}",
                    0o644,
                    (1000, 1000),
                );
            },
            vec![(
                crate::targets::LEFTOVERS,
                Finding::Leftovers {
                    paths: vec!["/home/alice/.local/state/mix/.state.mix-backup-r9-1".into()],
                },
            )],
        ),
        (
            "a repository root wrote into",
            |world: &mut World| {
                let head =
                    repository_dir(Path::new("/home/alice")).join(crate::world::REPOSITORY_HEAD);
                world.files.get_mut(&head).unwrap().owner = (0, 0);
            },
            vec![(
                ALICE_REPOSITORY,
                Finding::Owner {
                    actual: (0, 0),
                    expected: (1000, 1000),
                },
            )],
        ),
        (
            "an old generation whose link no longer resolves",
            |world: &mut World| {
                let profile = world.profiles.get_mut(&1000).unwrap();
                profile.generations.push(7);
                profile.dangling.push(7);
            },
            vec![(
                ALICE_GENERATIONS,
                Finding::GenerationDangling { generation: 7 },
            )],
        ),
        (
            "an active generation whose link no longer resolves",
            |world: &mut World| {
                let profile = world.profiles.get_mut(&1000).unwrap();
                let active = profile.active.unwrap();
                profile.dangling.push(active);
            },
            vec![(
                ALICE_GENERATIONS,
                Finding::GenerationDangling { generation: 1 },
            )],
        ),
        (
            "a file in the way of a managed one",
            |world: &mut World| {
                world
                    .clobbered
                    .insert(1000, vec![PathBuf::from("/home/alice/.bashrc")]);
            },
            vec![(
                "/home/alice",
                Finding::InTheWay {
                    paths: vec!["/home/alice/.bashrc".into()],
                },
            )],
        ),
        (
            "a missing runtime",
            |world: &mut World| {
                world.files.remove(Path::new(DEFAULT_PROFILE_NIX_ENV));
            },
            vec![("default profile", Finding::RuntimeMissing)],
        ),
    ]
}

#[test]
fn a_bootstrapped_machine_is_healthy() {
    let alice = user("alice", 1000);

    assert_eq!(
        audit(&bootstrapped(std::slice::from_ref(&alice)), &alice),
        []
    );
}

#[test]
fn every_finding_is_found_on_its_target_and_fixed_or_refused() {
    let alice = user("alice", 1000);
    let healthy = bootstrapped(std::slice::from_ref(&alice));

    for (what, drift, found) in drifts() {
        let mut world = healthy.clone();
        drift(&mut world);
        let expected: Vec<(String, Finding)> = found
            .iter()
            .map(|(target, finding)| (target.to_string(), finding.clone()))
            .collect();

        assert_eq!(audit(&world, &alice), expected, "{what}");
        let report = repair(&mut world, &alice);
        let left: Vec<(String, Finding)> = expected
            .iter()
            .filter(|(_, finding)| finding.unfixable().is_some())
            .cloned()
            .collect();
        for (target, finding) in &expected {
            let outcome = report
                .steps
                .iter()
                .find(|(step, _)| step == target)
                .map(|(_, outcome)| outcome.clone());
            match finding.unfixable() {
                None => assert_eq!(outcome, Some(StepOutcome::Changed), "{what}: {target}"),
                Some(reason) => assert!(
                    matches!(
                        &outcome,
                        Some(StepOutcome::Failed(Failure::Unrepairable { reason: found, .. }))
                            if *found == reason
                    ),
                    "{what}: {outcome:?}"
                ),
            }
        }
        assert_eq!(audit(&world, &alice), left, "{what} after repair");
    }
}

#[test]
fn an_unfixable_finding_does_not_stop_the_others_being_fixed() {
    let alice = user("alice", 1000);
    let mut world = bootstrapped(std::slice::from_ref(&alice));
    world.files.remove(Path::new(DEFAULT_PROFILE_NIX_ENV));
    world.files.remove(Path::new(POLICY_FILE));
    world.groups.get_mut("nixbld").unwrap().gid = 31_000;

    repair(&mut world, &alice);

    assert_eq!(
        audit(&world, &alice),
        [("default profile".to_string(), Finding::RuntimeMissing)]
    );
}

#[test]
fn a_leftover_an_unrecovered_request_may_need_is_kept() {
    let alice = user("alice", 1000);
    let mut world = bootstrapped(std::slice::from_ref(&alice));
    let backup = mix_state_dir(Path::new("/home/alice")).join(".state.mix-backup-r9-1");
    world.with_file(&backup, b"{}", 0o644, (1000, 1000));
    world.journals = vec!["r9".into()];

    let report = repair(&mut world, &alice);

    assert!(world.files.contains_key(&backup));
    assert!(report.steps.iter().any(|(step, outcome)| {
        step == crate::targets::LEFTOVERS
            && matches!(
                outcome,
                StepOutcome::Failed(Failure::Unrepairable {
                    reason: Unfixable::Unrecovered,
                    ..
                })
            )
    }));
}

#[test]
fn a_rewritten_nix_conf_restarts_a_running_daemon() {
    let alice = user("alice", 1000);
    let mut world = bootstrapped(std::slice::from_ref(&alice));
    world
        .apply(&Action::StartUnit {
            unit: NIX_DAEMON_SERVICE_UNIT.to_string(),
        })
        .unwrap();
    world.with_file(NIX_CONF_DEST, b"trusted-users = *\n", 0o644, (0, 0));

    let report = repair(&mut world, &alice);

    assert!(
        report
            .steps
            .contains(&(Cow::Borrowed(RESTART_NIX_DAEMON), StepOutcome::Changed))
    );
}

#[test]
fn repairing_one_user_leaves_the_other_alone() {
    let alice = user("alice", 1000);
    let bob = user("bob", 1001);
    let mut world = bootstrapped(&[alice.clone(), bob.clone()]);
    let bob_state = mix_state_dir(Path::new("/home/bob"));
    world.with_file(
        bob_state.join(crate::paths::HOME_NIX),
        b"changed by bob",
        0o644,
        (1001, 1001),
    );
    world.with_file(
        mix_state_dir(Path::new("/home/alice")).join(crate::paths::HOME_NIX),
        b"changed",
        0o644,
        (1000, 1000),
    );
    let before = world
        .files
        .get(&bob_state.join(crate::paths::HOME_NIX))
        .cloned();

    repair(&mut world, &alice);

    assert_eq!(audit(&world, &alice), []);
    assert_eq!(
        world
            .files
            .get(&bob_state.join(crate::paths::HOME_NIX))
            .cloned(),
        before
    );
}

#[test]
fn every_finding_and_category_reach_the_event_stream_as_their_own_kind() {
    let findings = [
        Finding::Missing,
        Finding::Unreadable {
            kind: std::io::ErrorKind::PermissionDenied,
        },
        Finding::NotADirectory,
        Finding::Mode {
            actual: 0o600,
            expected: 0o644,
        },
        Finding::Owner {
            actual: (0, 0),
            expected: (1000, 1000),
        },
        Finding::ContentDrift,
        Finding::GroupMissing,
        Finding::GroupGid {
            actual: 1,
            expected: 30000,
        },
        Finding::NotAMember { group: "mix-users" },
        Finding::NoSuchUser,
        Finding::UserMissing,
        Finding::UserIds {
            actual: (1, 2),
            expected: (30001, 30000),
        },
        Finding::UnitMissing,
        Finding::UnitDrift,
        Finding::UnitInactive,
        Finding::RuntimeMissing,
        Finding::RepositoryBroken,
        Finding::RepositoryLocked,
        Finding::Interrupted {
            requests: vec!["r1".into()],
        },
        Finding::Leftovers {
            paths: vec!["/etc/nix/.nix.conf.mix-backup-r1-1".into()],
        },
        Finding::GenerationDangling { generation: 3 },
        Finding::InTheWay {
            paths: vec!["/home/alice/.bashrc".into()],
        },
    ];
    let mut kinds = std::collections::HashSet::new();
    for finding in findings {
        let listed = match &finding {
            Finding::Missing
            | Finding::Unreadable { .. }
            | Finding::NotADirectory
            | Finding::Mode { .. }
            | Finding::Owner { .. }
            | Finding::ContentDrift
            | Finding::GroupMissing
            | Finding::GroupGid { .. }
            | Finding::NotAMember { .. }
            | Finding::NoSuchUser
            | Finding::UserMissing
            | Finding::UserIds { .. }
            | Finding::UnitMissing
            | Finding::UnitDrift
            | Finding::UnitInactive
            | Finding::RuntimeMissing
            | Finding::RepositoryBroken
            | Finding::RepositoryLocked
            | Finding::Interrupted { .. }
            | Finding::Leftovers { .. }
            | Finding::GenerationDangling { .. }
            | Finding::InTheWay { .. } => finding.clone(),
        };
        let kind = wire::finding(listed)
            .kind
            .expect("every finding has a kind");
        assert!(
            kinds.insert(std::mem::discriminant(&kind)),
            "{finding:?} shares its kind"
        );
    }
    for category in crate::targets::Category::ALL {
        assert_ne!(
            wire::category(category),
            mix_events::v1::Category::Unspecified
        );
    }
}

#[test]
fn a_report_tells_the_reader_what_repair_cannot_fix() {
    let report = |finding| HealthReport {
        name: "/nix".to_string(),
        category: crate::targets::Category::Filesystem,
        finding: Some(finding),
        drift: None,
    };

    assert_eq!(
        wire::report(&report(Finding::RuntimeMissing)).unfixable(),
        mix_events::v1::Unfixable::MissingRuntime
    );
    assert_eq!(
        wire::report(&report(Finding::Missing)).unfixable(),
        mix_events::v1::Unfixable::Unspecified
    );
}

#[test]
fn a_drift_is_the_lines_there_now_against_the_lines_mix_wrote() {
    let wrote = "build-users-group = nixbld\nmax-jobs = auto\nsandbox = true\n";
    let now = "build-users-group = nixbuild\nsandbox = true\ntrusted-users = ciuser\n";

    assert_eq!(
        hunks(wrote, now),
        [
            Hunk {
                found_line: 1,
                found: vec!["build-users-group = nixbuild".into()],
                expected: vec![
                    "build-users-group = nixbld".into(),
                    "max-jobs = auto".into()
                ],
            },
            Hunk {
                found_line: 3,
                found: vec!["trusted-users = ciuser".into()],
                expected: vec![],
            },
        ]
    );
}

#[test]
fn a_secret_setting_never_leaves_the_core_with_its_value() {
    let wrote = "sandbox = true\n";
    let now = "sandbox = true\naccess-tokens = github.com=ghp_secret\nextra-access-tokens=x=y\n";

    let drift = hunks(wrote, now);

    assert_eq!(
        drift[0].found,
        ["access-tokens = <hidden>", "extra-access-tokens = <hidden>"]
    );
    assert!(!format!("{drift:?}").contains("ghp_secret"));
}

#[test]
fn files_without_known_contents_have_no_drift_to_show() {
    let world = World::default();
    let target = Target::Directory {
        path: Path::new("/nix").into(),
        mode: 0o755,
        owner: None,
    };
    let facts: Vec<Fact> = queries(&target)
        .iter()
        .map(|query| world.observe(query))
        .collect();
    assert_eq!(drift(&target, &facts), None);
}
