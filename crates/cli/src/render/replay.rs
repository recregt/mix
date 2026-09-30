#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use std::collections::HashMap;
use std::sync::Arc;

use mix_events::capture::Captured;
use mix_events::v1::{Envelope, envelope::Event};
use mix_events::{Detail, NodeId};
use mix_shell::render::Render;
use mix_ui::Out;

use super::human::Human;

fn node_of(event: &Event) -> NodeId {
    match event {
        Event::NodeStarted(started) => started.id,
        Event::NodeFinished(finished) => finished.id,
        Event::NodeProgress(progress) => progress.id,
        Event::NotRun(not_run) => not_run.parent,
        Event::Diagnostic(diagnostic) => diagnostic.node,
    }
}

fn subtree(envelopes: &[Envelope], path: &str) -> Vec<NodeId> {
    let mut paths: HashMap<NodeId, String> = HashMap::new();
    for envelope in envelopes {
        if let Some(Event::NodeStarted(started)) = &envelope.event {
            let own = match paths.get(&started.parent) {
                Some(parent) => format!("{parent}/{}", started.key),
                None => started.key.clone(),
            };
            paths.insert(started.id, own);
        }
    }
    let prefix = format!("{path}/");
    paths
        .into_iter()
        .filter(|(_, own)| own == path || own.starts_with(&prefix))
        .map(|(id, _)| id)
        .collect()
}

pub fn show(captured: &Captured, level: Detail, node: Option<&str>, out: Arc<dyn Out>) {
    let kept = node.map(|path| subtree(&captured.envelopes, path));
    let mut human = Human::new(Arc::new(mix_ui::Silent)).level(level).to(out);
    for (envelope, offset) in captured.envelopes.iter().zip(&captured.offsets) {
        if let (Some(kept), Some(event)) = (&kept, &envelope.event)
            && !kept.contains(&node_of(event))
        {
            continue;
        }
        human.at(*offset);
        human.envelope(envelope.clone());
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    use mix_events::capture::Capture;
    use mix_events::capture::v1::Header;
    use mix_events::v1::command::Request;
    use mix_events::v1::node_progress::Progress;
    use mix_events::v1::node_started::Kind;
    use mix_events::v1::{
        Action, BuildStarted, Builds, Bytes, Cancellation, Command, CommandFinished,
        CommandStarted, Download, FetchStarted, Inspection, InstallRequest, InstallResult,
        Journaled, LockWait, NixActivity, NotRunReason, Observation, Observed, Operation,
        OutputLine, Plan, Process, Rollback, Sequenced, Step, Stopping, Stream, Verb, journaled,
        node_finished, observation,
    };
    use mix_events::{Ending, Outbox, ROOT, Start, Tree};

    use super::*;

    struct Buffer(Mutex<Vec<String>>);

    impl Out for Buffer {
        fn line(&self, text: &str) {
            self.0.lock().unwrap().push(text.to_string());
        }

        fn colours(&self) -> bool {
            false
        }
    }

    fn start(kind: Kind, key: &'static str) -> Start {
        Start::new(key, kind)
    }

    fn step(verb: Verb, subject: &str) -> Kind {
        Kind::Step(Step {
            verb: verb as i32,
            subject: subject.into(),
        })
    }

    fn action(operation: Operation, subject: &str) -> Kind {
        Kind::Action(Action {
            operation: operation as i32,
            subject: subject.into(),
        })
    }

    fn command() -> Start {
        Start::command(
            "install",
            Command {
                mix_version: "0.1.0".into(),
                schema_minor: mix_events::SCHEMA_MINOR,
                request: Some(Request::Install(InstallRequest {
                    packages: vec!["hello".into()],
                })),
            },
        )
    }

    fn installed() -> Vec<Envelope> {
        let outbox = Arc::new(Outbox::new("01920000-0000-7000-8000-000000000001", || {}));
        let mut tree = Tree::new(Arc::clone(&outbox), Arc::new(|| None), command());
        let lock = tree
            .start(
                ROOT,
                start(
                    Kind::LockWait(LockWait {
                        lock: "/var/lib/mix/lock".into(),
                        holder: None,
                    }),
                    "lock",
                ),
            )
            .unwrap();
        tree.finish(lock, Ending::succeeded()).unwrap();
        let plan = tree
            .start(
                ROOT,
                start(
                    Kind::Plan(Plan {
                        title: "install".into(),
                    }),
                    "plan",
                ),
            )
            .unwrap();
        let write = tree
            .start(
                plan,
                start(step(Verb::Writing, "package list"), "write-config"),
            )
            .unwrap();
        let put = tree
            .start(
                write,
                start(
                    action(Operation::PutFile, "/home/ciuser/.local/state/mix/state"),
                    "a1",
                ),
            )
            .unwrap();
        tree.progress(
            put,
            Progress::Observed(Observed {
                observations: vec![Observation {
                    query: Some(observation::Query::Contents(
                        "/home/ciuser/.local/state/mix/state".into(),
                    )),
                    fact: Some(observation::Fact::ContentsFact(
                        mix_events::v1::ContentsFact {
                            present: true,
                            length: 412,
                        },
                    )),
                }],
            }),
        )
        .unwrap();
        tree.progress(
            put,
            Progress::Journaled(Journaled {
                record: Some(journaled::Record::Prepared(Sequenced {
                    seq: 0,
                    undo: vec![Action {
                        operation: Operation::Restore as i32,
                        subject: "/home/ciuser/.local/state/mix/state".into(),
                    }],
                })),
            }),
        )
        .unwrap();
        tree.finish(put, Ending::succeeded()).unwrap();
        tree.finish(write, Ending::succeeded()).unwrap();
        let activate = tree
            .start(plan, start(step(Verb::Installing, "hello"), "activate"))
            .unwrap();
        let profile = tree
            .start(
                activate,
                start(action(Operation::ActivateProfile, "ciuser"), "a2"),
            )
            .unwrap();
        let process = tree
            .start(
                profile,
                start(
                    Kind::Process(Process {
                        program: "nix".into(),
                        args: vec!["build".into()],
                    }),
                    "p1",
                ),
            )
            .unwrap();
        tree.finish(process, Ending::succeeded()).unwrap();
        let download = tree
            .start(
                profile,
                start(
                    Kind::Download(Download {
                        artifact: "hello".into(),
                        url: "http://mirror.internal/hello.nar".into(),
                    }),
                    "d1",
                ),
            )
            .unwrap();
        tree.progress(
            download,
            Progress::Fetch(FetchStarted {
                url: "http://mirror.internal/hello.nar".into(),
            }),
        )
        .unwrap();
        tree.progress(
            download,
            Progress::Bytes(Bytes {
                done: 1024,
                total: Some(2048),
            }),
        )
        .unwrap();
        tree.finish(download, Ending::succeeded()).unwrap();
        let activity = tree
            .start(
                profile,
                start(
                    Kind::NixActivity(NixActivity {
                        activity_type: "build".into(),
                        text: "building hello".into(),
                    }),
                    "n1",
                ),
            )
            .unwrap();
        tree.finish(activity, Ending::succeeded()).unwrap();
        for progress in [
            Progress::Command(CommandStarted {
                line: "/nix/var/nix/profiles/default/bin/nix build".into(),
            }),
            Progress::Build(BuildStarted {
                derivation: "/nix/store/bbx79xgf89bvd25i1sivdcykhy39bz14-hello-2.12.drv".into(),
            }),
            Progress::Builds(Builds {
                builds_done: 1,
                builds_expected: 3,
                ..Builds::default()
            }),
            Progress::Line(OutputLine {
                text: "building hello".into(),
                stream: Stream::Stderr as i32,
            }),
            Progress::CommandFinished(CommandFinished {
                line: "/nix/var/nix/profiles/default/bin/nix build".into(),
                exit_code: Some(0),
                signal: None,
            }),
        ] {
            tree.progress(profile, progress).unwrap();
        }
        tree.finish(profile, Ending::succeeded()).unwrap();
        tree.warn(
            activate,
            mix_core::diagnose::warning(
                mix_events::v1::Code::GitRecordFailed,
                "could not record the change in git",
                &mix_core::action::Failure::CommandFailed {
                    program: "git commit".into(),
                    status: Some(128),
                    output_tail: "fatal: not a git repository".into(),
                },
            ),
        )
        .unwrap();
        tree.finish(activate, Ending::succeeded()).unwrap();
        tree.finish(plan, Ending::succeeded()).unwrap();
        tree.finish(
            ROOT,
            Ending::succeeded()
                .with_result(node_finished::Result::Install(InstallResult {
                    added: vec!["hello".into()],
                    ..InstallResult::default()
                }))
                .for_root(false),
        )
        .unwrap();
        drop(tree);
        outbox.drain()
    }

    fn interrupted() -> Vec<Envelope> {
        let outbox = Arc::new(Outbox::new("01920000-0000-7000-8000-000000000002", || {}));
        let stopped: mix_events::Stopped = Arc::new(|| Some(Cancellation::Interrupted));
        let mut tree = Tree::new(Arc::clone(&outbox), stopped, command());
        let plan = tree
            .start(
                ROOT,
                start(
                    Kind::Plan(Plan {
                        title: "install".into(),
                    }),
                    "plan",
                )
                .planned(["write-config", "activate"]),
            )
            .unwrap();
        let inspection = tree
            .start(
                plan,
                start(
                    Kind::Inspection(Inspection {
                        target: "/nix".into(),
                        category: mix_events::v1::Category::Filesystem as i32,
                    }),
                    "i1",
                ),
            )
            .unwrap();
        tree.finish(inspection, Ending::succeeded()).unwrap();
        let write = tree
            .start(
                plan,
                start(step(Verb::Writing, "package list"), "write-config"),
            )
            .unwrap();
        let put = tree
            .start(
                write,
                start(
                    action(Operation::PutFile, "/home/ciuser/.local/state/mix/state"),
                    "a1",
                ),
            )
            .unwrap();
        tree.progress(
            put,
            Progress::CommandFinished(CommandFinished {
                line: "/usr/bin/git add".into(),
                exit_code: None,
                signal: Some(2),
            }),
        )
        .unwrap();
        tree.finish(put, Ending::succeeded()).unwrap();
        tree.finish(write, Ending::succeeded()).unwrap();
        tree.progress(
            ROOT,
            Progress::Stopping(Stopping {
                cause: Cancellation::Interrupted as i32,
            }),
        )
        .unwrap();
        let rollback = tree
            .start(
                plan,
                start(
                    Kind::Rollback(Rollback { undoes: write }),
                    "undo-write-config",
                ),
            )
            .unwrap();
        let restore = tree
            .start(
                rollback,
                start(
                    action(Operation::Restore, "/home/ciuser/.local/state/mix/state"),
                    "u1",
                ),
            )
            .unwrap();
        tree.finish(restore, Ending::succeeded()).unwrap();
        tree.finish(rollback, Ending::succeeded()).unwrap();
        tree.not_run(plan, "activate", NotRunReason::NotReached)
            .unwrap();
        tree.finish(plan, Ending::cancelled(Cancellation::Interrupted))
            .unwrap();
        tree.finish(
            ROOT,
            Ending::cancelled(Cancellation::Interrupted).for_root(false),
        )
        .unwrap();
        drop(tree);
        outbox.drain()
    }

    fn slot(envelope: &Envelope) -> Option<usize> {
        Some(match envelope.event.as_ref()? {
            Event::NodeStarted(started) => match started.kind.as_ref()? {
                Kind::Command(_) => 0,
                Kind::Plan(_) => 1,
                Kind::Step(_) => 2,
                Kind::Rollback(_) => 3,
                Kind::Process(_) => 4,
                Kind::Download(_) => 5,
                Kind::Inspection(_) => 6,
                Kind::LockWait(_) => 7,
                Kind::NixActivity(_) => 8,
                Kind::Action(_) => 9,
            },
            Event::NodeFinished(_) => 10,
            Event::NotRun(_) => 11,
            Event::Diagnostic(_) => 12,
            Event::NodeProgress(progress) => match progress.progress.as_ref()? {
                Progress::Bytes(_) => 13,
                Progress::Builds(_) => 14,
                Progress::Line(_) => 15,
                Progress::Command(_) => 16,
                Progress::Fetch(_) => 17,
                Progress::Build(_) => 18,
                Progress::Stopping(_) => 19,
                Progress::CommandFinished(_) => 20,
                Progress::Observed(_) => 21,
                Progress::Journaled(_) => 22,
            },
        })
    }

    const SLOTS: usize = 23;

    fn captured(envelopes: &[Envelope]) -> (Vec<u8>, Captured) {
        let mut capture = Capture::start(
            Vec::new(),
            Header {
                format: String::new(),
                mix_version: "0.1.0".into(),
                request: envelopes[0].request.clone(),
                started: None,
            },
        )
        .unwrap();
        for (second, envelope) in envelopes.iter().enumerate() {
            capture
                .record(Duration::from_secs(second as u64), envelope)
                .unwrap();
        }
        let bytes = capture.into_inner();
        let read = mix_events::capture::read(bytes.as_slice()).unwrap();
        (bytes, read)
    }

    fn rendered(captured: &Captured, level: Detail, node: Option<&str>) -> String {
        let out = Arc::new(Buffer(Mutex::new(Vec::new())));
        show(captured, level, node, out.clone());
        let lines = out.0.lock().unwrap().join("\n");
        lines + "\n"
    }

    fn golden(name: &str, observed: &[u8]) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/events")
            .join(name);
        if std::env::var("MIX_UPDATE_GOLDEN").is_ok_and(|value| value == "1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, observed).unwrap();
        }
        let expected =
            std::fs::read(&path).unwrap_or_else(|_| panic!("{} is missing", path.display()));
        assert_eq!(
            String::from_utf8_lossy(observed),
            String::from_utf8_lossy(&expected),
            "{}",
            path.display()
        );
    }

    #[test]
    fn every_kind_of_event_renders_at_every_level_as_recorded() {
        let mut seen = [false; SLOTS];
        for (name, envelopes) in [("installed", installed()), ("interrupted", interrupted())] {
            mix_events::validate(envelopes.iter()).unwrap();
            for index in envelopes.iter().filter_map(slot) {
                seen[index] = true;
            }
            let (bytes, captured) = captured(&envelopes);
            golden(&format!("{name}.ndjson"), &bytes);
            for (level, suffix) in [
                (Detail::Outcome, "quiet"),
                (Detail::Step, "default"),
                (Detail::Action, "v"),
                (Detail::Trace, "vv"),
            ] {
                golden(
                    &format!("{name}-{suffix}.txt"),
                    rendered(&captured, level, None).as_bytes(),
                );
            }
        }
        let missing: Vec<usize> = (0..SLOTS).filter(|index| !seen[*index]).collect();
        assert_eq!(missing, Vec::<usize>::new(), "slots no fixture shows");
    }

    #[test]
    fn a_node_is_shown_with_what_ran_inside_it_and_nothing_else() {
        let (_, captured) = captured(&installed());

        let shown = rendered(&captured, Detail::Trace, Some("install/plan/activate"));

        assert!(shown.contains("Installing hello"), "{shown}");
        assert!(shown.contains("Running `/nix/var/nix/profiles/default/bin/nix build`"));
        assert!(!shown.contains("package list"), "{shown}");
    }
}
