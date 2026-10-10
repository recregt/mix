use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use insta::assert_json_snapshot;
use mix_events::v1::command::Request;
use mix_events::v1::{Command, NotRunReason, Operation, Status};
use mix_events::{Outbox, Outcome as EventOutcome, ROOT, Start, Tree, validate};
use proptest::prelude::*;

use super::*;
use crate::effect::{Expect, Kind as PathKind};
use crate::model::World;
use crate::model::testkit::{self, Run, Script, recover, requested};

struct EnsureDir {
    key: &'static str,
    path: PathBuf,
    mode: u32,
}

impl StepSpec for EnsureDir {
    fn key(&self) -> Cow<'static, str> {
        self.key.into()
    }

    fn title(&self) -> crate::run::Title {
        crate::run::Title::new(mix_events::v1::Verb::Creating, "ensure a directory")
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Path(self.path.clone())]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok({
            let Fact::Path(facts) = &facts[0] else {
                unreachable!()
            };
            match facts.kind {
                PathKind::Missing => vec![Action::CreateDir {
                    path: self.path.clone(),
                    mode: self.mode,
                    owner: None,
                }],
                _ if facts.mode != self.mode => vec![Action::SetMode {
                    path: self.path.clone(),
                    mode: self.mode,
                    expect: facts.mode,
                }],
                _ => Vec::new(),
            }
        })
    }
}

struct EnsureFile {
    key: &'static str,
    path: PathBuf,
    contents: &'static str,
    shielded: bool,
}

impl StepSpec for EnsureFile {
    fn key(&self) -> Cow<'static, str> {
        self.key.into()
    }

    fn title(&self) -> crate::run::Title {
        crate::run::Title::new(mix_events::v1::Verb::Creating, "ensure a file")
    }

    fn queries(&self) -> Vec<Query> {
        vec![
            Query::Path(self.path.clone()),
            Query::Contents(self.path.clone()),
        ]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok((|| -> Vec<Action> {
            let (Fact::Path(path), Fact::Contents(contents)) = (&facts[0], &facts[1]) else {
                unreachable!()
            };
            if contents.as_deref() == Some(self.contents.as_bytes()) {
                return Vec::new();
            }
            vec![Action::PutFile {
                path: self.path.clone(),
                contents: Arc::from(self.contents.as_bytes()),
                mode: 0o644,
                owner: None,
                expect: path.id.map_or(Expect::Absent, Expect::Present),
            }]
        })())
    }

    fn shielded(&self) -> bool {
        self.shielded
    }
}

struct EnsureGroup {
    key: &'static str,
    name: &'static str,
    gid: u32,
}

impl StepSpec for EnsureGroup {
    fn key(&self) -> Cow<'static, str> {
        self.key.into()
    }

    fn title(&self) -> crate::run::Title {
        crate::run::Title::new(mix_events::v1::Verb::Creating, "ensure a group")
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Group(self.name.to_string())]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok({
            match &facts[0] {
                Fact::Group(None) => vec![Action::AddGroup {
                    name: self.name.to_string(),
                    gid: self.gid,
                }],
                Fact::Group(Some(group)) if group.gid != self.gid => vec![Action::SetGroupGid {
                    name: self.name.to_string(),
                    gid: self.gid,
                    expect: group.gid,
                }],
                _ => Vec::new(),
            }
        })
    }
}

fn bootstrap_like() -> Vec<Box<dyn StepSpec>> {
    vec![
        Box::new(EnsureDir {
            key: "create-nix-dir",
            path: "/nix".into(),
            mode: 0o755,
        }),
        Box::new(EnsureDir {
            key: "create-nix-var",
            path: "/nix/var".into(),
            mode: 0o755,
        }),
        Box::new(EnsureGroup {
            key: "create-groups",
            name: "nixbld",
            gid: 30_000,
        }),
        Box::new(EnsureFile {
            key: "write-marker",
            path: "/nix/.mix-managed".into(),
            contents: "",
            shielded: false,
        }),
        Box::new(EnsureFile {
            key: "write-nix-conf",
            path: "/etc/nix.conf".into(),
            contents: "trusted-users = root\n",
            shielded: false,
        }),
    ]
}

fn drive(world: &mut World, steps: Vec<Box<dyn StepSpec>>, script: Script) -> Run {
    drive_runner(world, Runner::new(ROOT, steps), script)
}

fn drive_runner(world: &mut World, runner: Runner, script: Script) -> Run {
    testkit::drive(
        world,
        runner,
        requested(Request::Bootstrap(Box::default())),
        &script,
    )
}

fn outcome(run: &Run, path: &str) -> Option<EventOutcome> {
    validate(&run.stream).unwrap().outcome(path)
}

fn forward_actions(world: &World) -> usize {
    let mut world = world.clone();
    drive(&mut world, bootstrap_like(), Script::default())
        .performed
        .iter()
        .filter(|action| **action != Action::Commit)
        .count()
}

#[test]
fn a_fresh_run_does_every_step_and_commits() {
    let mut world = World::default();
    let before = world.clone();

    let run = drive(&mut world, bootstrap_like(), Script::default());

    assert_json_snapshot!(run.case(&before, &world));
    assert!(world.pending().is_empty());
}

#[test]
fn every_action_and_every_undo_is_a_node_under_what_ran_it() {
    let mut world = World::default();

    let run = drive(&mut world, bootstrap_like(), Script::failing_at(1));

    let started: Vec<(String, ActionNode)> = run
        .stream
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Some(mix_events::v1::envelope::Event::NodeStarted(node)) => match &node.kind {
                Some(Kind::Action(action)) => Some((node.key.clone(), action.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let described = |operation: Operation, subject: &str| ActionNode {
        operation: operation as i32,
        subject: subject.to_string(),
    };
    assert_eq!(
        started,
        [
            (
                "action-1".to_string(),
                described(Operation::CreateDir, "/nix")
            ),
            (
                "action-2".to_string(),
                described(Operation::CreateDir, "/nix/var")
            ),
            (
                "action-3".to_string(),
                described(Operation::RemoveCreated, "/nix")
            ),
        ]
    );
    for (path, status) in [
        ("bootstrap/plan/create-nix-dir/action-1", Status::Succeeded),
        ("bootstrap/plan/create-nix-var/action-2", Status::Failed),
        (
            "bootstrap/plan/rollback:create-nix-dir/action-3",
            Status::Succeeded,
        ),
    ] {
        assert_eq!(
            outcome(&run, path),
            Some(EventOutcome::Finished(status)),
            "{path}"
        );
    }
}

#[test]
fn an_independent_step_that_fails_is_undone_alone_and_the_others_still_run() {
    let mut world = World::default();

    let before = world.clone();

    let run = drive_runner(
        &mut world,
        Runner::new(ROOT, bootstrap_like()).independent(),
        Script::failing_at(1),
    );

    assert_json_snapshot!(run.case(&before, &world));
    assert!(world.pending().is_empty());
    assert!(validate(&run.stream).is_ok());
}

#[test]
fn an_independent_run_that_is_stopped_undoes_the_step_in_progress_and_keeps_the_rest() {
    let mut world = World::default();

    let before = world.clone();

    let run = drive_runner(
        &mut world,
        Runner::new(ROOT, bootstrap_like()).independent(),
        Script::stopping_after(1),
    );

    assert_json_snapshot!(run.case(&before, &world));
    assert!(world.pending().is_empty());
    assert_eq!(
        outcome(&run, "bootstrap/plan/create-groups"),
        Some(EventOutcome::NotRun(NotRunReason::NotReached))
    );
    assert!(validate(&run.stream).is_ok());
}

#[test]
fn a_second_run_finds_everything_satisfied_and_does_nothing() {
    let mut world = World::default();
    drive(&mut world, bootstrap_like(), Script::default());
    let before = world.clone();

    let run = drive(&mut world, bootstrap_like(), Script::default());

    assert_json_snapshot!(run.case(&before, &world));
    assert_eq!(world, before);
}

#[test]
fn a_failure_at_any_action_leaves_the_world_as_it_was() {
    let base = World::default();
    for fail_at in 0..forward_actions(&base) {
        let mut world = base.clone();

        let run = drive(&mut world, bootstrap_like(), Script::failing_at(fail_at));

        assert!(
            matches!(run.report().verdict, Verdict::Failed { .. }),
            "{fail_at}: {:?}",
            run.report().verdict
        );
        assert!(run.report().rollback_failures.is_empty());
        assert_eq!(world, base, "failing at action {fail_at}");
    }
}

#[test]
fn a_stop_at_any_point_rolls_back_and_reports_what_was_not_reached() {
    let base = World::default();
    for stop_after in 0..forward_actions(&base) {
        let mut world = base.clone();

        let run = drive(
            &mut world,
            bootstrap_like(),
            Script::stopping_after(stop_after),
        );

        assert_eq!(
            run.report().verdict,
            Verdict::Cancelled(Cancellation::Interrupted),
            "{stop_after}"
        );
        assert_eq!(world, base, "stopping after action {stop_after}");
    }
    let mut world = base.clone();
    let run = drive(&mut world, bootstrap_like(), Script::stopping_after(0));
    assert_eq!(
        outcome(&run, "bootstrap/plan/write-nix-conf"),
        Some(EventOutcome::NotRun(NotRunReason::NotReached))
    );
    assert_eq!(
        outcome(&run, "bootstrap/plan/rollback:create-nix-dir"),
        Some(EventOutcome::Finished(Status::Succeeded))
    );
}

#[test]
fn a_failed_undo_is_reported_the_others_still_run_and_nothing_foreign_is_removed() {
    let mut world = World::default();
    let before = world.clone();
    let last = forward_actions(&world) - 1;

    let run = drive(
        &mut world,
        bootstrap_like(),
        Script {
            fail_undo_at: Some(0),
            ..Script::failing_at(last)
        },
    );

    assert_json_snapshot!(run.case(&before, &world));
    let failed: Vec<&str> = run
        .report()
        .rollback_failures
        .iter()
        .map(|(step, _)| step.as_ref())
        .collect();
    assert_eq!(failed, ["write-marker", "create-nix-dir"]);
    assert!(matches!(
        run.report().rollback_failures[1].1,
        Failure::Conflict { .. }
    ));
    assert!(
        !world.groups.contains_key("nixbld"),
        "the group was still removed"
    );
    assert!(!world.files.contains_key(std::path::Path::new("/nix/var")));
    assert!(
        world
            .files
            .contains_key(std::path::Path::new("/nix/.mix-managed"))
    );
    let validated = validate(&run.stream).unwrap();
    assert_eq!(
        validated.outcome("bootstrap/plan/rollback:create-nix-dir"),
        Some(EventOutcome::Finished(Status::Failed))
    );
    assert_eq!(
        validated.outcome("bootstrap/plan/rollback:create-groups"),
        Some(EventOutcome::Finished(Status::Succeeded))
    );
}

#[test]
fn a_commit_that_fails_still_succeeds_and_warns() {
    let mut world = World::default();
    world.with_file("/etc/nix.conf", b"legacy", 0o644, (0, 0));
    let before = world.clone();

    let run = drive(
        &mut world,
        bootstrap_like(),
        Script {
            fail_commit: true,
            failure: Failure::Io {
                path: "/etc/.nix.conf.mix-backup-1".into(),
                kind: std::io::ErrorKind::PermissionDenied,
            },
            ..Script::default()
        },
    );

    assert_json_snapshot!(run.case(&before, &world));
    assert_eq!(world.pending().len(), 1);
}

#[test]
fn a_shielded_step_finishes_before_a_stop_takes_effect() {
    let mut world = World::default();
    let steps: Vec<Box<dyn StepSpec>> = vec![
        Box::new(EnsureFile {
            key: "activate",
            path: "/etc/first".into(),
            contents: "a",
            shielded: true,
        }),
        Box::new(EnsureFile {
            key: "later",
            path: "/etc/second".into(),
            contents: "b",
            shielded: false,
        }),
    ];

    let before = world.clone();

    let run = drive(&mut world, steps, Script::stopping_after(0));

    assert_json_snapshot!(run.case(&before, &world));
    assert_eq!(
        outcome(&run, "bootstrap/plan/later"),
        Some(EventOutcome::NotRun(NotRunReason::NotReached))
    );
    assert_eq!(world.contents("/etc/first"), None);
}

fn random_steps() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0u8..6, 1..8)
}

fn build(kinds: &[u8]) -> Vec<Box<dyn StepSpec>> {
    const KEYS: [&str; 8] = ["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7"];
    kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| -> Box<dyn StepSpec> {
            let key = KEYS[index];
            match kind {
                0 => Box::new(EnsureDir {
                    key,
                    path: "/nix".into(),
                    mode: 0o755,
                }),
                1 => Box::new(EnsureDir {
                    key,
                    path: "/var/empty".into(),
                    mode: 0o555,
                }),
                2 => Box::new(EnsureFile {
                    key,
                    path: "/etc/nix.conf".into(),
                    contents: "one",
                    shielded: false,
                }),
                3 => Box::new(EnsureFile {
                    key,
                    path: "/etc/nix.conf".into(),
                    contents: "two",
                    shielded: index % 2 == 0,
                }),
                4 => Box::new(EnsureGroup {
                    key,
                    name: "nixbld",
                    gid: 30_000,
                }),
                _ => Box::new(EnsureGroup {
                    key,
                    name: "nixbld",
                    gid: 30_001,
                }),
            }
        })
        .collect()
}

proptest! {
    #[test]
    fn any_plan_either_completes_and_commits_or_leaves_the_world_as_it_was(
        kinds in random_steps(),
        preexisting in any::<bool>(),
        fail_at in prop::option::of(0usize..10),
        stop_after in prop::option::of(0usize..10),
    ) {
        let mut base = World::default();
        if preexisting {
            base.with_file("/etc/nix.conf", b"legacy", 0o644, (0, 0));
        }
        let mut world = base.clone();

        let run = drive(&mut world, build(&kinds), Script { fail_at, stop_after, ..Script::default() });

        match run.report().verdict {
            Verdict::Succeeded => {
                prop_assert!(world.pending().is_empty());
                let again = drive(&mut world.clone(), build(&kinds[kinds.len() - 1..]), Script::default());
                prop_assert!(again.performed.is_empty(), "{:?}", again.performed);
            }
            _ => prop_assert_eq!(&world, &base),
        }
        prop_assert!(run.report().rollback_failures.is_empty());
    }
}

#[test]
fn a_step_that_cannot_be_observed_fails_and_rolls_back_what_came_before() {
    let base = World::default();
    let mut world = base.clone();

    let run = drive(
        &mut world,
        bootstrap_like(),
        Script {
            unobservable: Some(|queries| {
                matches!(queries.first(), Some(Query::Group(_)))
                    .then_some(Failure::SystemdUnreachable)
            }),
            ..Script::default()
        },
    );

    assert_eq!(
        run.report().verdict,
        Verdict::Failed {
            step: "create-groups".into(),
            failure: Failure::SystemdUnreachable
        }
    );
    assert_eq!(world, base);
}

#[test]
fn every_undo_is_performed_shielded_and_no_forward_action_is() {
    let mut world = World::default();

    let run = drive(
        &mut world,
        bootstrap_like(),
        Script::failing_when(
            |action| matches!(action, Action::PutFile { path, .. } if path.ends_with("nix.conf")),
            Script::default().failure,
        ),
    );
    let performed: Vec<(Action, bool)> = run
        .performed
        .iter()
        .cloned()
        .zip(run.shielded.iter().copied())
        .collect();

    let failed_at = performed
        .iter()
        .position(|(action, _)| matches!(action, Action::PutFile { path, .. } if path.ends_with("nix.conf")))
        .unwrap();
    assert!(
        performed[..=failed_at]
            .iter()
            .all(|(_, shielded)| !shielded)
    );
    assert!(
        performed[failed_at + 1..]
            .iter()
            .all(|(_, shielded)| *shielded)
    );
    assert!(performed.len() > failed_at + 1);
}

#[test]
fn a_crash_before_or_after_any_change_is_recovered_to_the_start_or_the_finish() {
    let mut base = World::default();
    base.with_file("/etc/nix.conf", b"legacy", 0o644, (0, 0));
    let mut finished = base.clone();
    drive(&mut finished, bootstrap_like(), Script::default());
    let changes = forward_actions(&base) + 1;

    for crash_at in 0..changes {
        for after_change in [false, true] {
            let mut world = base.clone();
            let crashed = drive(
                &mut world,
                bootstrap_like(),
                Script::crashing(crash_at, after_change),
            );

            recover(&mut world, &crashed.journal);

            let committing = crashed
                .journal
                .contains(&crate::run::journal::Record::Committing);
            let expected = if committing { &finished } else { &base };
            assert_eq!(
                &world, expected,
                "crash at change {crash_at}, after the change: {after_change}"
            );
        }
    }
}

#[test]
fn an_action_that_failed_after_taking_effect_is_undone_as_one_in_doubt() {
    let base = World::default();
    for failing in 0..forward_actions(&base) {
        let mut world = base.clone();

        let run = drive(
            &mut world,
            bootstrap_like(),
            Script {
                in_doubt_at: Some(failing),
                ..Script::default()
            },
        );

        let report = run.report();
        assert!(matches!(report.verdict, Verdict::Cancelled(_)), "{failing}");
        assert!(
            report.rollback_failures.is_empty(),
            "{failing}: {:?}",
            report.rollback_failures
        );
        assert_eq!(world, base, "failing at {failing}");
    }
}

#[test]
fn a_finished_plan_stepped_again_stays_finished() {
    let mut world = World::default();
    let outbox = Arc::new(Outbox::new("plan", || {}));
    let mut tree = Tree::new(
        outbox.clone(),
        Arc::new(|| None),
        Start::command("bootstrap", Command::default()),
    );
    let mut plan = Runner::new(ROOT, bootstrap_like());
    make_guard!(guard);
    let mut runner = plan.brand(guard);
    let mut input = None;
    let closed = loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(Ok(queries
                    .iter()
                    .map(|query| world.observe(query))
                    .collect())));
            }
            Next::Perform(action) => input = Some(Input::Done(world.apply(&action))),
            Next::Finished(closed) => break closed,
        }
    };
    let events = outbox.drain().len();

    let Next::Finished(_) = runner.step(&mut tree, None) else {
        panic!("a finished plan stays finished");
    };
    let first = runner.report(closed).clone();
    make_guard!(later);
    let mut runner = plan.brand(later);
    let Next::Finished(again) = runner.step(&mut tree, None) else {
        panic!("a finished plan stays finished");
    };

    assert_eq!(outbox.drain().len(), 0);
    assert!(events > 0);
    assert_eq!(first.verdict, Verdict::Succeeded);
    assert_eq!(runner.report(again), &first);
}
