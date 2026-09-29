use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_events::v1::{Command, Envelope, NotRunReason, Status};
use mix_events::{Ending, Outbox, Outcome as EventOutcome, ROOT, Start, Tree, validate};
use proptest::prelude::*;

use super::*;
use crate::action::{Expect, Kind as PathKind};
use crate::model::World;

struct EnsureDir {
    key: &'static str,
    path: PathBuf,
    mode: u32,
}

impl StepSpec for EnsureDir {
    fn key(&self) -> Cow<'static, str> {
        self.key.into()
    }

    fn title(&self) -> Cow<'static, str> {
        "ensure a directory".into()
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

    fn title(&self) -> Cow<'static, str> {
        "ensure a file".into()
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

    fn title(&self) -> Cow<'static, str> {
        "ensure a group".into()
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

#[derive(Debug, Clone, Copy, Default)]
struct Script {
    fail_at: Option<usize>,
    stop_after: Option<usize>,
    fail_undo_at: Option<usize>,
}

struct Run {
    report: Report,
    stream: Vec<Envelope>,
    performed: Vec<Action>,
}

fn drive(world: &mut World, steps: Vec<Box<dyn StepSpec>>, script: Script) -> Run {
    drive_runner(world, Runner::new(ROOT, steps), script)
}

fn drive_runner(world: &mut World, mut runner: Runner, script: Script) -> Run {
    let outbox = Arc::new(Outbox::new("request", || {}));
    let mut tree = Tree::new(
        outbox.clone(),
        Arc::new(|| None),
        Start::command("bootstrap", Command::default()),
    );
    let mut input = None;
    let mut performed = Vec::new();
    let mut forward = 0;
    let mut undone = 0;
    let mut rolling_back = false;
    let report = loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(Ok(queries
                    .iter()
                    .map(|query| world.observe(query))
                    .collect())));
            }
            Next::Perform(action) => {
                performed.push(action.clone());
                let outcome = if rolling_back {
                    undone += 1;
                    if script.fail_undo_at == Some(undone - 1) {
                        Err(Failure::Io {
                            path: "/injected".into(),
                            kind: std::io::ErrorKind::Other,
                        })
                    } else {
                        world.apply(&action)
                    }
                } else if action == Action::Commit {
                    world.apply(&action)
                } else {
                    forward += 1;
                    if script.fail_at == Some(forward - 1) {
                        rolling_back = true;
                        Err(Failure::Io {
                            path: "/injected".into(),
                            kind: std::io::ErrorKind::Other,
                        })
                    } else {
                        let outcome = world.apply(&action);
                        if script.stop_after == Some(forward - 1) {
                            runner.stop(Cancellation::Interrupted);
                            rolling_back = true;
                        }
                        outcome
                    }
                };
                input = Some(Input::Done(outcome));
            }
            Next::Finished(report) => break report,
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
        performed,
    }
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

    let run = drive(&mut world, bootstrap_like(), Script::default());

    assert_eq!(run.report.verdict, Verdict::Succeeded);
    assert_eq!(run.performed.last(), Some(&Action::Commit));
    assert!(world.pending().is_empty());
    assert_eq!(
        outcome(&run, "bootstrap/plan/write-nix-conf"),
        Some(EventOutcome::Finished(Status::Succeeded))
    );
}

#[test]
fn every_action_and_every_undo_is_a_node_under_what_ran_it() {
    let mut world = World::default();

    let run = drive(
        &mut world,
        bootstrap_like(),
        Script {
            fail_at: Some(1),
            ..Script::default()
        },
    );

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

    let run = drive_runner(
        &mut world,
        Runner::new(ROOT, bootstrap_like()).independent(),
        Script {
            fail_at: Some(1),
            ..Script::default()
        },
    );

    assert!(
        matches!(&run.report.verdict, Verdict::Failed { step, .. } if step == "create-nix-var"),
        "{:?}",
        run.report.verdict
    );
    let outcomes: Vec<(&str, bool)> = run
        .report
        .steps
        .iter()
        .map(|(step, outcome)| (step.as_ref(), matches!(outcome, StepOutcome::Changed)))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("create-nix-dir", true),
            ("create-nix-var", false),
            ("create-groups", true),
            ("write-marker", true),
            ("write-nix-conf", true),
        ]
    );
    assert!(world.files.contains_key(Path::new("/nix")));
    assert!(!world.files.contains_key(Path::new("/nix/var")));
    assert!(world.pending().is_empty());
    assert!(validate(&run.stream).is_ok());
}

#[test]
fn an_independent_run_that_is_stopped_undoes_the_step_in_progress_and_keeps_the_rest() {
    let mut world = World::default();

    let run = drive_runner(
        &mut world,
        Runner::new(ROOT, bootstrap_like()).independent(),
        Script {
            stop_after: Some(1),
            ..Script::default()
        },
    );

    assert!(matches!(run.report.verdict, Verdict::Cancelled(_)));
    assert!(world.files.contains_key(Path::new("/nix")));
    assert!(!world.files.contains_key(Path::new("/nix/var")));
    assert!(!world.groups.contains_key("nixbld"));
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

    assert!(run.performed.is_empty(), "{:?}", run.performed);
    assert_eq!(world, before);
    for step in ["create-nix-dir", "create-groups", "write-nix-conf"] {
        assert_eq!(
            outcome(&run, &format!("bootstrap/plan/{step}")),
            Some(EventOutcome::Finished(Status::AlreadySatisfied))
        );
    }
}

#[test]
fn a_failure_at_any_action_leaves_the_world_as_it_was() {
    let base = World::default();
    for fail_at in 0..forward_actions(&base) {
        let mut world = base.clone();

        let run = drive(
            &mut world,
            bootstrap_like(),
            Script {
                fail_at: Some(fail_at),
                ..Script::default()
            },
        );

        assert!(
            matches!(run.report.verdict, Verdict::Failed { .. }),
            "{fail_at}: {:?}",
            run.report.verdict
        );
        assert!(run.report.rollback_failures.is_empty());
        assert_eq!(world, base, "failing at action {fail_at}");
        assert!(validate(&run.stream).is_ok());
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
        assert_eq!(world, base, "stopping after action {stop_after}");
        assert!(validate(&run.stream).is_ok());
    }
    let mut world = base.clone();
    let run = drive(
        &mut world,
        bootstrap_like(),
        Script {
            stop_after: Some(0),
            ..Script::default()
        },
    );
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
    let last = forward_actions(&world) - 1;

    let run = drive(
        &mut world,
        bootstrap_like(),
        Script {
            fail_at: Some(last),
            fail_undo_at: Some(0),
            ..Script::default()
        },
    );

    let failed: Vec<&str> = run
        .report
        .rollback_failures
        .iter()
        .map(|(step, _)| step.as_ref())
        .collect();
    assert_eq!(failed, ["write-marker", "create-nix-dir"]);
    assert!(matches!(
        run.report.rollback_failures[1].1,
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
    let outbox = Arc::new(Outbox::new("request", || {}));
    let mut tree = Tree::new(
        outbox.clone(),
        Arc::new(|| None),
        Start::command("bootstrap", Command::default()),
    );
    let mut runner = Runner::new(ROOT, bootstrap_like());
    let mut input = None;
    let report = loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(Ok(queries
                    .iter()
                    .map(|query| world.observe(query))
                    .collect())));
            }
            Next::Perform(Action::Commit) => {
                input = Some(Input::Done(Err(Failure::Io {
                    path: "/etc/.nix.conf.mix-backup-1".into(),
                    kind: std::io::ErrorKind::PermissionDenied,
                })));
            }
            Next::Perform(action) => input = Some(Input::Done(world.apply(&action))),
            Next::Finished(report) => break report,
        }
    };
    tree.finish(ROOT, Ending::succeeded()).unwrap();
    drop(tree);
    let stream = outbox.drain();

    assert_eq!(report.verdict, Verdict::Succeeded);
    assert_eq!(world.pending().len(), 1);
    assert!(validate(&stream).is_ok());
    assert!(stream.iter().any(|envelope| matches!(
        &envelope.event,
        Some(mix_events::v1::envelope::Event::Diagnostic(_))
    )));
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

    let run = drive(
        &mut world,
        steps,
        Script {
            stop_after: Some(0),
            ..Script::default()
        },
    );

    assert_eq!(
        outcome(&run, "bootstrap/plan/activate"),
        Some(EventOutcome::Finished(Status::Succeeded))
    );
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

        let run = drive(&mut world, build(&kinds), Script { fail_at, stop_after, fail_undo_at: None });

        prop_assert!(validate(&run.stream).is_ok(), "{:?}", validate(&run.stream));
        match run.report.verdict {
            Verdict::Succeeded => {
                prop_assert!(world.pending().is_empty());
                let again = drive(&mut world.clone(), build(&kinds[kinds.len() - 1..]), Script::default());
                prop_assert!(again.performed.is_empty(), "{:?}", again.performed);
            }
            _ => prop_assert_eq!(&world, &base),
        }
        prop_assert!(run.report.rollback_failures.is_empty());
    }
}

fn answer(
    runner: &mut Runner,
    world: &mut World,
    observe: impl Fn(&World, &[Query]) -> Result<Vec<Fact>, Failure>,
    mut fail: impl FnMut(&Action) -> bool,
) -> (Report, Vec<(Action, bool)>) {
    let outbox = Arc::new(Outbox::new("request", || {}));
    let mut tree = Tree::new(
        outbox,
        Arc::new(|| None),
        Start::command("bootstrap", Command::default()),
    );
    let mut input = None;
    let mut performed = Vec::new();
    loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => input = Some(Input::Facts(observe(world, &queries))),
            Next::Perform(action) => {
                performed.push((action.clone(), runner.shielded()));
                let outcome = if fail(&action) {
                    Err(Failure::Io {
                        path: "/injected".into(),
                        kind: std::io::ErrorKind::Other,
                    })
                } else {
                    world.apply(&action)
                };
                input = Some(Input::Done(outcome));
            }
            Next::Finished(report) => return (report, performed),
        }
    }
}

#[test]
fn a_step_that_cannot_be_observed_fails_and_rolls_back_what_came_before() {
    let base = World::default();
    let mut world = base.clone();
    let mut runner = Runner::new(ROOT, bootstrap_like());

    let (report, _) = answer(
        &mut runner,
        &mut world,
        |world, queries| {
            if matches!(queries.first(), Some(Query::Group(_))) {
                Err(Failure::SystemdUnreachable)
            } else {
                Ok(queries.iter().map(|query| world.observe(query)).collect())
            }
        },
        |_| false,
    );

    assert_eq!(
        report.verdict,
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
    let mut runner = Runner::new(ROOT, bootstrap_like());

    let (_, performed) = answer(
        &mut runner,
        &mut world,
        |world, queries| Ok(queries.iter().map(|query| world.observe(query)).collect()),
        |action| matches!(action, Action::PutFile { path, .. } if path.ends_with("nix.conf")),
    );

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

fn crashing_run(
    world: &mut World,
    crash_at: usize,
    after_change: bool,
) -> Vec<crate::journal::Record> {
    use crate::journal::Record;
    let outbox = Arc::new(Outbox::new("request", || {}));
    let mut tree = Tree::new(
        outbox,
        Arc::new(|| None),
        Start::command("bootstrap", Command::default()),
    );
    let mut runner = Runner::new(ROOT, bootstrap_like());
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
                records.push(Record::Prepared {
                    seq,
                    undo: undo.clone(),
                });
                if seq as usize == crash_at && !after_change {
                    return records;
                }
                let outcome = world.apply(&action);
                if seq as usize == crash_at && after_change {
                    return records;
                }
                records.push(Record::Done { seq });
                if action == Action::Commit {
                    records.push(Record::Ended);
                }
                seq += 1;
                input = Some(Input::Done(outcome));
            }
            Next::Finished(_) => return records,
        }
    }
}

fn recovered(world: &mut World, records: &[crate::journal::Record]) {
    use crate::journal::{Recovery, recover};
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
            let records = crashing_run(&mut world, crash_at, after_change);

            recovered(&mut world, &records);

            let committing = records.contains(&crate::journal::Record::Committing);
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
        let outbox = Arc::new(Outbox::new("request", || {}));
        let mut tree = Tree::new(
            outbox,
            Arc::new(|| None),
            Start::command("bootstrap", Command::default()),
        );
        let mut runner = Runner::new(ROOT, bootstrap_like());
        let mut input = None;
        let mut forward = 0;
        let report = loop {
            match runner.step(&mut tree, input.take()) {
                Next::Observe(queries) => {
                    input = Some(Input::Facts(Ok(queries
                        .iter()
                        .map(|query| world.observe(query))
                        .collect())));
                }
                Next::Perform(action) => {
                    let outcome = world.apply(&action);
                    let counts = !runner.rolling_back() && action != Action::Commit;
                    input = Some(Input::Done(if counts && forward == failing {
                        runner.in_doubt(outcome.expect("the action applies").undo);
                        Err(Failure::Cancelled)
                    } else {
                        outcome
                    }));
                    if counts {
                        forward += 1;
                    }
                }
                Next::Finished(report) => break report,
            }
        };

        assert!(matches!(report.verdict, Verdict::Cancelled(_)), "{failing}");
        assert!(
            report.rollback_failures.is_empty(),
            "{failing}: {:?}",
            report.rollback_failures
        );
        assert_eq!(world, base, "failing at {failing}");
    }
}
