use std::path::PathBuf;
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
    fn key(&self) -> &'static str {
        self.key
    }

    fn title(&self) -> &'static str {
        "ensure a directory"
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Path(self.path.clone())]
    }

    fn actions(&self, facts: &[Fact]) -> Vec<Action> {
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
    }
}

struct EnsureFile {
    key: &'static str,
    path: PathBuf,
    contents: &'static str,
    shielded: bool,
}

impl StepSpec for EnsureFile {
    fn key(&self) -> &'static str {
        self.key
    }

    fn title(&self) -> &'static str {
        "ensure a file"
    }

    fn queries(&self) -> Vec<Query> {
        vec![
            Query::Path(self.path.clone()),
            Query::Contents(self.path.clone()),
        ]
    }

    fn actions(&self, facts: &[Fact]) -> Vec<Action> {
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
    fn key(&self) -> &'static str {
        self.key
    }

    fn title(&self) -> &'static str {
        "ensure a group"
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Group(self.name.to_string())]
    }

    fn actions(&self, facts: &[Fact]) -> Vec<Action> {
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
    let outbox = Arc::new(Outbox::new("request", || {}));
    let mut tree = Tree::new(
        outbox.clone(),
        Arc::new(|| None),
        Start::command("bootstrap", Command::default()),
    );
    let mut runner = Runner::new(ROOT, steps);
    let mut input = None;
    let mut performed = Vec::new();
    let mut forward = 0;
    let mut undone = 0;
    let mut rolling_back = false;
    let report = loop {
        match runner.step(&mut tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(
                    queries.iter().map(|query| world.observe(query)).collect(),
                ));
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
        .map(|(step, _)| *step)
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
                input = Some(Input::Facts(
                    queries.iter().map(|query| world.observe(query)).collect(),
                ));
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
