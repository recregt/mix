use std::sync::{Arc, Mutex};
use std::time::Duration;

use mix_events::v1::{
    Bytes, Command, Diagnostic, Envelope, Log, NotRunReason, Plan, Rollback, Status, Step,
    envelope::Event, node_progress, node_started,
};
use mix_events::{Clock, Ending, Node, Outcome, Sink, Stamper, Validated, Violation, validate};
use proptest::prelude::*;

struct Fixed;

impl Clock for Fixed {
    fn wall(&self) -> pbjson_types::Timestamp {
        pbjson_types::Timestamp {
            seconds: 1_790_000_000,
            nanos: 0,
        }
    }

    fn monotonic(&self) -> Duration {
        Duration::ZERO
    }
}

fn record(run: impl FnOnce(Arc<dyn Sink>)) -> Vec<Envelope> {
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let into = delivered.clone();
    let sink: Arc<dyn Sink> = Arc::new(Stamper::new("request", Fixed, move |envelope| {
        into.lock().unwrap().push(envelope)
    }));
    run(sink);
    Arc::try_unwrap(delivered).unwrap().into_inner().unwrap()
}

fn root(sink: Arc<dyn Sink>, planned: &[&str]) -> Node<'static> {
    Node::root(
        sink,
        Arc::new(|| false),
        "bootstrap",
        Command::default(),
        planned.iter().map(|key| key.to_string()).collect(),
    )
}

fn step() -> node_started::Kind {
    node_started::Kind::Step(Step::default())
}

fn renumber(stream: &mut [Envelope]) {
    for (index, envelope) in stream.iter_mut().enumerate() {
        envelope.seq = index as u64 + 1;
    }
}

fn interrupted_bootstrap() -> Vec<Envelope> {
    record(|sink| {
        let root = root(sink, &["plan"]);
        let plan = root.planned_child(
            "plan",
            node_started::Kind::Plan(Plan::default()),
            ["create-nix-dir", "fetch-runtime", "write-nix-conf"]
                .map(String::from)
                .to_vec(),
        );
        let created = plan.child("create-nix-dir", step());
        let created_id = created.id();
        created.finish(Ending::succeeded());
        let fetch = plan.child("fetch-runtime", step());
        let download = fetch.child("download", node_started::Kind::Download(Default::default()));
        download.progress(node_progress::Progress::Bytes(Bytes { done: 1, total: 2 }));
        download.finish(Ending::cancelled());
        fetch.finish(Ending::cancelled());
        plan.child(
            "rollback:create-nix-dir",
            node_started::Kind::Rollback(Rollback { undoes: created_id }),
        )
        .finish(Ending::succeeded());
        plan.finish(Ending::cancelled());
        root.finish(Ending::cancelled().with_exit_code(130));
    })
}

#[test]
fn an_interrupted_bootstrap_reads_as_a_tree_of_paths() {
    let validated = validate(&interrupted_bootstrap()).unwrap();

    let cancelled = Outcome::Finished(Status::Cancelled);
    assert_eq!(validated.outcome("bootstrap"), Some(cancelled));
    assert_eq!(
        validated.outcome("bootstrap/plan/create-nix-dir"),
        Some(Outcome::Finished(Status::Succeeded))
    );
    assert_eq!(
        validated.outcome("bootstrap/plan/fetch-runtime/download"),
        Some(cancelled)
    );
    assert_eq!(
        validated.outcome("bootstrap/plan/write-nix-conf"),
        Some(Outcome::NotRun(NotRunReason::NotReached))
    );
    assert_eq!(
        validated.outcome("bootstrap/plan/rollback:create-nix-dir"),
        Some(Outcome::Finished(Status::Succeeded))
    );
}

fn edited(edit: impl FnOnce(&mut Vec<Envelope>)) -> Result<Validated, Violation> {
    let mut stream = interrupted_bootstrap();
    edit(&mut stream);
    validate(&stream)
}

fn event_mut(stream: &mut [Envelope], index: usize) -> &mut Event {
    stream[index].event.as_mut().unwrap()
}

fn position(stream: &[Envelope], matches: impl Fn(&Event) -> bool) -> usize {
    stream
        .iter()
        .position(|envelope| envelope.event.as_ref().is_some_and(&matches))
        .unwrap()
}

fn finish_of(id: u64) -> impl Fn(&Event) -> bool {
    move |event| matches!(event, Event::NodeFinished(node) if node.id == id)
}

#[test]
fn a_stream_without_the_root_finishing_is_truncated() {
    assert_eq!(
        edited(|stream| {
            stream.pop();
        }),
        Err(Violation::Truncated { open: vec![1] })
    );
}

#[test]
fn a_stream_that_does_not_open_with_the_root_is_refused() {
    assert_eq!(
        edited(|stream| {
            stream.remove(0);
            renumber(stream);
        }),
        Err(Violation::FirstNotRoot { seq: 1 })
    );
}

#[test]
fn a_node_that_starts_under_a_finished_parent_is_refused() {
    let result = edited(|stream| {
        let fetch = position(stream, finish_of(4));
        let started = stream[fetch].clone();
        let Some(Event::NodeFinished(_)) = &started.event else {
            unreachable!()
        };
        let late = Envelope {
            event: Some(Event::NodeStarted(mix_events::v1::NodeStarted {
                id: 99,
                parent: 4,
                key: "late".to_string(),
                planned: vec![],
                kind: Some(step()),
            })),
            ..started
        };
        stream.insert(fetch + 1, late);
        renumber(stream);
    });

    assert_eq!(result, Err(Violation::ParentFinished { id: 99, parent: 4 }));
}

#[test]
fn a_rollback_must_undo_a_finished_step_beside_it() {
    let result = edited(|stream| {
        let index = position(
            stream,
            |event| matches!(event, Event::NodeStarted(node) if matches!(node.kind, Some(node_started::Kind::Rollback(_)))),
        );
        if let Event::NodeStarted(node) = event_mut(stream, index) {
            node.kind = Some(node_started::Kind::Rollback(Rollback { undoes: 5 }));
        }
    });

    assert_eq!(result, Err(Violation::RollbackTarget { id: 6, undoes: 5 }));
}

#[test]
fn a_negative_elapsed_time_is_refused() {
    let result = edited(|stream| {
        let index = position(stream, finish_of(3));
        if let Event::NodeFinished(node) = event_mut(stream, index) {
            node.elapsed = Some(pbjson_types::Duration {
                seconds: -1,
                nanos: 0,
            });
        }
    });

    assert_eq!(result, Err(Violation::NegativeElapsed { id: 3 }));
}

#[test]
fn only_the_root_carries_an_exit_code() {
    let result = edited(|stream| {
        let index = position(stream, finish_of(3));
        if let Event::NodeFinished(node) = event_mut(stream, index) {
            node.exit_code = 1;
        }
    });

    assert_eq!(result, Err(Violation::ExitCodeOnChild { id: 3 }));
}

#[test]
fn a_finish_without_a_status_is_refused() {
    let result = edited(|stream| {
        let index = position(stream, finish_of(3));
        if let Event::NodeFinished(node) = event_mut(stream, index) {
            node.status = Status::Unspecified as i32;
        }
    });

    assert_eq!(result, Err(Violation::UnspecifiedStatus { id: 3 }));
}

#[test]
fn progress_for_a_finished_node_is_refused() {
    let result = edited(|stream| {
        let index = position(stream, finish_of(5));
        let mut progress = stream[index].clone();
        progress.event = Some(Event::NodeProgress(mix_events::v1::NodeProgress {
            id: 5,
            progress: None,
        }));
        stream.insert(index + 1, progress);
        renumber(stream);
    });

    assert_eq!(result, Err(Violation::NotOpen { id: 5 }));
}

#[test]
fn a_log_may_belong_to_the_request_rather_than_a_node() {
    let result = edited(|stream| {
        let mut log = stream[1].clone();
        log.event = Some(Event::Log(Log::default()));
        stream.insert(2, log);
        renumber(stream);
    });

    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn an_event_kind_from_a_newer_schema_is_skipped() {
    let result = edited(|stream| {
        let mut unknown = stream[1].clone();
        unknown.event = None;
        stream.insert(2, unknown);
        renumber(stream);
    });

    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_warning_points_at_a_running_node() {
    let result = edited(|stream| {
        let mut warning = stream[1].clone();
        warning.event = Some(Event::Diagnostic(Diagnostic {
            node: 42,
            ..Diagnostic::default()
        }));
        stream.insert(2, warning);
        renumber(stream);
    });

    assert_eq!(result, Err(Violation::NotOpen { id: 42 }));
}

#[derive(Debug, Clone, Copy)]
enum End {
    Succeeded,
    Satisfied,
    Failed,
    Cancelled,
    Dropped,
}

#[derive(Debug, Clone)]
enum Op {
    Child(Shape),
    NotRun(bool),
    Progress,
    Warn,
}

#[derive(Debug, Clone)]
struct Shape {
    planned: bool,
    rollback: bool,
    unreached: u8,
    end: End,
    ops: Vec<Op>,
}

fn end() -> impl Strategy<Value = End> {
    prop_oneof![
        Just(End::Succeeded),
        Just(End::Satisfied),
        Just(End::Failed),
        Just(End::Cancelled),
        Just(End::Dropped),
    ]
}

fn shape() -> impl Strategy<Value = Shape> {
    let leaf = (any::<bool>(), any::<bool>(), 0u8..3, end()).prop_map(
        |(planned, rollback, unreached, end)| Shape {
            planned,
            rollback,
            unreached,
            end,
            ops: vec![],
        },
    );
    leaf.prop_recursive(4, 48, 6, |inner| {
        (
            any::<bool>(),
            any::<bool>(),
            0u8..3,
            end(),
            prop::collection::vec(
                prop_oneof![
                    4 => inner.prop_map(Op::Child),
                    1 => any::<bool>().prop_map(Op::NotRun),
                    1 => Just(Op::Progress),
                    1 => Just(Op::Warn),
                ],
                0..6,
            ),
        )
            .prop_map(|(planned, rollback, unreached, end, ops)| Shape {
                planned,
                rollback,
                unreached,
                end,
                ops,
            })
    })
}

fn busy() -> impl Strategy<Value = Shape> {
    (shape(), shape()).prop_map(|(mut shape, child)| {
        shape.ops.insert(0, Op::Child(child));
        shape.ops.push(Op::NotRun(false));
        shape
    })
}

fn plan_of(shape: &Shape) -> Vec<String> {
    let mut planned: Vec<String> = shape
        .ops
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, Op::Child(Shape { planned: true, .. }) | Op::NotRun(_)))
        .map(|(index, _)| format!("k{index}"))
        .collect();
    planned.extend((0..shape.unreached).map(|index| format!("p{index}")));
    planned
}

fn run(node: &Node<'_>, ops: &[Op]) {
    let mut last_step = None;
    for (index, op) in ops.iter().enumerate() {
        let key = format!("k{index}");
        match op {
            Op::Child(shape) => {
                let kind = match (shape.rollback, last_step) {
                    (true, Some(undoes)) => node_started::Kind::Rollback(Rollback { undoes }),
                    _ => step(),
                };
                let is_step = matches!(kind, node_started::Kind::Step(_));
                let child = node.planned_child(key, kind, plan_of(shape));
                run(&child, &shape.ops);
                if is_step {
                    last_step = Some(child.id());
                }
                end_node(child, shape.end);
            }
            Op::NotRun(skipped) => node.not_run(
                key,
                if *skipped {
                    NotRunReason::Skipped
                } else {
                    NotRunReason::NotReached
                },
            ),
            Op::Progress => node.progress(node_progress::Progress::Bytes(Bytes::default())),
            Op::Warn => node.warn(Diagnostic::default()),
        }
    }
}

fn end_node(node: Node<'_>, end: End) {
    match end {
        End::Succeeded => node.finish(Ending::succeeded()),
        End::Satisfied => node.finish(Ending::already_satisfied()),
        End::Failed => node.finish(Ending::failed(Diagnostic::default())),
        End::Cancelled => node.finish(Ending::cancelled()),
        End::Dropped => drop(node),
    }
}

fn produced(shape: &Shape) -> Vec<Envelope> {
    record(|sink| {
        let root = Node::root(
            sink,
            Arc::new(|| false),
            "command",
            Command::default(),
            plan_of(shape),
        );
        run(&root, &shape.ops);
        end_node(root, shape.end);
    })
}

fn indices(stream: &[Envelope], matches: impl Fn(&Event) -> bool) -> Vec<usize> {
    stream
        .iter()
        .enumerate()
        .filter(|(_, envelope)| envelope.event.as_ref().is_some_and(&matches))
        .map(|(index, _)| index)
        .collect()
}

proptest! {
    #[test]
    fn whatever_the_producer_emits_is_valid(shape in shape()) {
        let stream = produced(&shape);

        let validated = validate(&stream).map_err(|violation| TestCaseError::fail(violation.to_string()))?;

        let nodes = indices(&stream, |event| matches!(event, Event::NodeStarted(_)));
        let not_run = indices(&stream, |event| matches!(event, Event::NotRun(_)));
        prop_assert_eq!(validated.entries.len(), nodes.len() + not_run.len());
        prop_assert!(validated.entries.iter().all(|entry| entry.outcome != Outcome::Running));
    }

    #[test]
    fn a_missing_event_number_is_refused(shape in shape(), pick in any::<prop::sample::Index>()) {
        let mut stream = produced(&shape);
        let index = pick.index(stream.len());
        stream[index].seq += 1;

        let expected = index as u64 + 1;
        prop_assert_eq!(validate(&stream), Err(Violation::SeqGap { expected, found: expected + 1 }));
    }

    #[test]
    fn an_event_from_another_request_is_refused(shape in shape(), pick in any::<prop::sample::Index>()) {
        let mut stream = produced(&shape);
        prop_assume!(stream.len() > 1);
        let index = 1 + pick.index(stream.len() - 1);
        stream[index].request = "another".to_string();

        prop_assert_eq!(validate(&stream), Err(Violation::RequestChanged { seq: index as u64 + 1 }));
    }

    #[test]
    fn a_node_started_twice_is_refused(shape in busy(), pick in any::<prop::sample::Index>()) {
        let mut stream = produced(&shape);
        let starts = indices(&stream, |event| matches!(event, Event::NodeStarted(node) if node.id != 1));
        prop_assume!(!starts.is_empty());
        let index = *pick.get(&starts);
        let id = match &stream[index].event {
            Some(Event::NodeStarted(node)) => node.id,
            _ => unreachable!(),
        };
        stream.insert(index + 1, stream[index].clone());
        renumber(&mut stream);

        prop_assert_eq!(validate(&stream), Err(Violation::DuplicateId { id }));
    }

    #[test]
    fn a_child_left_running_keeps_its_parent_from_finishing(shape in busy(), pick in any::<prop::sample::Index>()) {
        let mut stream = produced(&shape);
        let finishes = indices(&stream, |event| matches!(event, Event::NodeFinished(node) if node.id != 1));
        prop_assume!(!finishes.is_empty());
        let index = *pick.get(&finishes);
        let child = match &stream[index].event {
            Some(Event::NodeFinished(node)) => node.id,
            _ => unreachable!(),
        };
        stream.remove(index);
        renumber(&mut stream);

        let result = validate(&stream);
        prop_assert!(
            matches!(result, Err(Violation::ChildrenOpen { child: open, .. }) if open == child)
                || matches!(result, Err(Violation::NotOpen { id }) if id == child)
                || matches!(result, Err(Violation::RollbackTarget { undoes, .. }) if undoes == child),
            "{:?}", result
        );
    }

    #[test]
    fn a_planned_child_that_vanished_is_refused(shape in busy(), pick in any::<prop::sample::Index>()) {
        let mut stream = produced(&shape);
        let not_run = indices(&stream, |event| matches!(event, Event::NotRun(_)));
        prop_assume!(!not_run.is_empty());
        let index = *pick.get(&not_run);
        let (parent, key) = match &stream[index].event {
            Some(Event::NotRun(not_run)) => (not_run.parent, not_run.key.clone()),
            _ => unreachable!(),
        };
        let planned = stream.iter().any(|envelope| matches!(
            &envelope.event,
            Some(Event::NodeStarted(node)) if node.id == parent && node.planned.contains(&key)
        ));
        prop_assume!(planned);
        stream.remove(index);
        renumber(&mut stream);

        prop_assert_eq!(validate(&stream), Err(Violation::PlannedMissing { id: parent, key }));
    }

    #[test]
    fn a_stream_cut_short_is_truncated(shape in shape(), pick in any::<prop::sample::Index>()) {
        let mut stream = produced(&shape);
        let keep = pick.index(stream.len());
        stream.truncate(keep);

        let result = validate(&stream);
        prop_assert!(matches!(result, Err(Violation::Truncated { .. })), "{:?}", result);
    }
}
