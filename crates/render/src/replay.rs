use std::sync::Mutex;
use std::time::Duration;

use mix_events::capture::Capture;
use mix_events::capture::v1::Header;
use mix_events::v1::command::Request;
use mix_events::v1::node_progress::Progress;
use mix_events::v1::node_started::Kind;
use mix_events::v1::{
    Action, BuildStarted, Builds, Bytes, Cancellation, Command, CommandFinished, CommandStarted,
    Download, FetchStarted, Inspection, InstallRequest, InstallResult, Journaled, LockWait,
    NixActivity, NotRunReason, Observation, Observed, Operation, OutputLine, Plan, Process,
    Rollback, Sequenced, Step, Stopping, Stream, SubstitutionStarted, Verb, journaled,
    node_finished, observation,
};
use mix_events::{Ending, Outbox, ROOT, Start, Tree};

use std::sync::Arc;

use mix_events::Detail;
use mix_events::Render;
use mix_events::capture::Captured;
use mix_events::v1::{Envelope, envelope::Event};
use mix_ui::Out;

use crate::human::Human;

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
            dry_run: false,
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
                    holder: Some("alice".into()),
                    command: Some("install".into()),
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
            start(action(Operation::ActivateProfile, "ciuser's profile"), "a2"),
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
        Progress::Waiting(LockWait {
            lock: "/home/ciuser/.local/state/nix/profiles/profile.lock".into(),
            holder: Some("ciuser".into()),
            command: Some("nix-env -i hello".into()),
        }),
        Progress::Command(CommandStarted {
            line: "/nix/var/nix/profiles/default/bin/nix build".into(),
        }),
        Progress::Substitution(SubstitutionStarted {
            path: "/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-glibc-2.40".into(),
            name: "glibc-2.40".into(),
        }),
        Progress::Build(BuildStarted {
            derivation: "/nix/store/bbx79xgf89bvd25i1sivdcykhy39bz14-hello-2.12.drv".into(),
            name: "hello-2.12".into(),
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
        mix_core::report::diagnose::warning(
            mix_events::v1::Code::GitRecordFailed,
            "could not record the change in git",
            &mix_core::effect::Failure::CommandFailed {
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

fn failed() -> Vec<Envelope> {
    let outbox = Arc::new(Outbox::new("01920000-0000-7000-8000-000000000004", || {}));
    let mut tree = Tree::new(Arc::clone(&outbox), Arc::new(|| None), command());
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
    tree.finish(put, Ending::succeeded()).unwrap();
    tree.finish(write, Ending::succeeded()).unwrap();
    let activate = tree
        .start(plan, start(step(Verb::Installing, "hello"), "activate"))
        .unwrap();
    let profile = tree
        .start(
            activate,
            start(action(Operation::ActivateProfile, "ciuser's profile"), "a2"),
        )
        .unwrap();
    let failure = mix_core::report::diagnose::command_failure(
        "/nix/var/nix/profiles/default/bin/nix build",
        Some(1),
        "error: attribute 'hello' missing\n       at /nix/store/x5piy362vlnbxc71zd6alpswvgsdsv55-source/home.nix:10:7:\n            9|       pkgs.git\n           10|       pkgs.hello\n             |       ^\n           11|     ];\n       Did you mean hello2?\n",
        "nix build failed".into(),
    );
    tree.finish(profile, Ending::failed(failure.clone()))
        .unwrap();
    tree.finish(activate, Ending::failed(failure.clone()))
        .unwrap();
    let undo_activate = tree
        .start(
            plan,
            start(
                Kind::Rollback(Rollback { undoes: activate }),
                "undo-activate",
            ),
        )
        .unwrap();
    tree.finish(undo_activate, Ending::succeeded()).unwrap();
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
    tree.finish(plan, Ending::failed(failure.clone())).unwrap();
    tree.finish(ROOT, Ending::failed(failure).for_root(false))
        .unwrap();
    drop(tree);
    outbox.drain()
}

fn drift(target: &str) -> Option<mix_core::ops::health::Drift> {
    let hunk = |found_line, found: &[&str], expected: &[&str]| mix_core::ops::health::Hunk {
        found_line,
        found: found.iter().map(|line| line.to_string()).collect(),
        expected: expected.iter().map(|line| line.to_string()).collect(),
    };
    let (path, hunks) = match target {
        "nix-daemon.service" => (
            "/etc/systemd/system/nix-daemon.service",
            vec![hunk(18, &["Nice=10"], &[])],
        ),
        "/etc/nix/nix.conf" => (
            "/etc/nix/nix.conf",
            vec![
                hunk(
                    1,
                    &["build-users-group = nixbuild"],
                    &["build-users-group = nixbld"],
                ),
                hunk(3, &[], &["max-jobs = auto"]),
                hunk(4, &["trusted-users = ciuser"], &[]),
            ],
        ),
        _ => return None,
    };
    Some(mix_core::ops::health::Drift {
        path: path.into(),
        hunks,
    })
}

fn doctored(
    findings: &[(&str, Option<mix_core::ops::health::Finding>)],
    blocked: &[(&str, &str)],
) -> Vec<Envelope> {
    let outbox = Arc::new(Outbox::new("01920000-0000-7000-8000-000000000005", || {}));
    let mut tree = Tree::new(
        Arc::clone(&outbox),
        Arc::new(|| None),
        Start::command(
            "doctor",
            Command {
                mix_version: "0.1.0".into(),
                schema_minor: mix_events::SCHEMA_MINOR,
                dry_run: false,
                request: Some(Request::Doctor(mix_events::v1::DoctorRequest::default())),
            },
        ),
    );
    let result = mix_events::v1::DoctorResult {
        reports: findings
            .iter()
            .map(|(target, finding)| {
                mix_core::report::inspection::report(&mix_core::ops::health::HealthReport {
                    name: (*target).into(),
                    category: mix_core::Category::Filesystem,
                    finding: finding.clone(),
                    drift: drift(target),
                    blocked_by: None,
                })
            })
            .chain(blocked.iter().map(|(target, by)| {
                mix_core::report::inspection::report(&mix_core::ops::health::HealthReport {
                    name: (*target).into(),
                    category: mix_core::Category::Filesystem,
                    finding: None,
                    drift: None,
                    blocked_by: Some((*by).into()),
                })
            }))
            .collect(),
    };
    let problems = findings.iter().any(|(_, finding)| finding.is_some());
    tree.finish(
        ROOT,
        Ending::succeeded()
            .with_result(node_finished::Result::Doctor(result))
            .for_root(problems),
    )
    .unwrap();
    drop(tree);
    outbox.drain()
}

#[test]
fn doctor_reads_at_every_level_as_recorded() {
    use mix_core::ops::health::Finding;

    let healthy = doctored(&[("/nix", None), ("nix-daemon.service", None)], &[]);
    let problems = &[
        ("/nix", None),
        ("/etc/nix/nix.conf", Some(Finding::ContentDrift)),
        ("nix-daemon.service", Some(Finding::UnitDrift)),
        (
            "/nix/var/nix/profiles",
            Some(Finding::Mode {
                actual: 0o700,
                expected: 0o755,
            }),
        ),
        ("default profile", Some(Finding::RuntimeMissing)),
    ];
    let problems = doctored(
        problems,
        &[
            ("/nix/var/nix/profiles/per-user", "/nix/var/nix/profiles"),
            ("nix-daemon.socket", "default profile"),
        ],
    );
    for (name, envelopes) in [("doctor-healthy", healthy), ("doctor-problems", problems)] {
        mix_events::validate(envelopes.iter()).unwrap();
        let (_, captured) = captured(&envelopes);
        for (level, suffix) in [
            (Detail::Outcome, "quiet"),
            (Detail::Step, "default"),
            (Detail::Action, "v"),
        ] {
            golden(
                &format!("{name}-{suffix}"),
                rendered(&captured, level).as_bytes(),
            );
        }
    }
}

fn repair_run(reports: Vec<mix_events::v1::RepairReport>) -> Vec<Envelope> {
    let outbox = Arc::new(Outbox::new("01920000-0000-7000-8000-000000000006", || {}));
    let mut tree = Tree::new(
        Arc::clone(&outbox),
        Arc::new(|| None),
        Start::command(
            "repair",
            Command {
                mix_version: "0.1.0".into(),
                schema_minor: mix_events::SCHEMA_MINOR,
                dry_run: false,
                request: Some(Request::Repair(mix_events::v1::RepairRequest::default())),
            },
        ),
    );
    let problems = reports.iter().any(|report| !report.fixed);
    tree.finish(
        ROOT,
        Ending::succeeded()
            .with_result(node_finished::Result::Repair(
                mix_events::v1::RepairResult { reports },
            ))
            .for_root(problems),
    )
    .unwrap();
    drop(tree);
    outbox.drain()
}

#[test]
fn a_repair_names_what_it_left_alone_and_why() {
    let envelopes = repair_run(vec![
        mix_events::v1::RepairReport {
            target: "/etc/nix/nix.conf".into(),
            fixed: true,
            failure: None,
            blocked_by: String::new(),
        },
        mix_events::v1::RepairReport {
            target: "/home/alice/.local/state/mix".into(),
            fixed: false,
            failure: Some(mix_events::v1::Diagnostic {
                code: mix_events::v1::Code::Io as i32,
                severity: mix_events::v1::Severity::Error as i32,
                message: "/home/alice/.local/state/mix: permission denied".into(),
                ..Default::default()
            }),
            blocked_by: String::new(),
        },
        mix_events::v1::RepairReport {
            target: "/home/alice/.local/state/mix/home.nix".into(),
            fixed: false,
            failure: None,
            blocked_by: "/home/alice/.local/state/mix".into(),
        },
    ]);
    mix_events::validate(envelopes.iter()).unwrap();
    let (_, captured) = captured(&envelopes);
    golden(
        "repair-blocked",
        rendered(&captured, Detail::Step).as_bytes(),
    );
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
            Progress::Substitution(_) => 23,
            Progress::Waiting(_) => 24,
        },
    })
}

const SLOTS: usize = 25;

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

fn rendered(captured: &Captured, level: Detail) -> String {
    let out = Arc::new(Buffer(Mutex::new(Vec::new())));
    let mut human = Human::new(Arc::new(mix_ui::Silent))
        .level(level)
        .to(out.clone());
    for (envelope, offset) in captured.envelopes.iter().zip(&captured.offsets) {
        human.at(*offset);
        human.envelope(envelope.clone());
    }
    out.0
        .lock()
        .unwrap()
        .iter()
        .map(|line| format!("{line}\n"))
        .collect()
}

fn golden(name: &str, observed: &[u8]) {
    insta::assert_snapshot!(
        name,
        std::str::from_utf8(observed).expect("what mix prints is UTF-8")
    );
}

#[test]
fn every_kind_of_event_renders_at_every_level_as_recorded() {
    let mut seen = [false; SLOTS];
    for (name, envelopes) in [
        ("installed", installed()),
        ("interrupted", interrupted()),
        ("failed", failed()),
    ] {
        mix_events::validate(envelopes.iter()).unwrap();
        for index in envelopes.iter().filter_map(slot) {
            seen[index] = true;
        }
        let (bytes, captured) = captured(&envelopes);
        golden(&format!("{name}-events"), &bytes);
        for (level, suffix) in [
            (Detail::Outcome, "quiet"),
            (Detail::Step, "default"),
            (Detail::Action, "v"),
            (Detail::Trace, "vv"),
        ] {
            golden(
                &format!("{name}-{suffix}"),
                rendered(&captured, level).as_bytes(),
            );
        }
    }
    let missing: Vec<usize> = (0..SLOTS).filter(|index| !seen[*index]).collect();
    assert_eq!(missing, Vec::<usize>::new(), "slots no fixture shows");
}

#[test]
fn a_warning_without_words_of_its_own_is_a_bug_and_its_producers_words_are_the_cause() {
    let outbox = Arc::new(Outbox::new("01920000-0000-7000-8000-000000000003", || {}));
    let mut tree = Tree::new(Arc::clone(&outbox), Arc::new(|| None), command());
    tree.warn(
        ROOT,
        mix_events::v1::Diagnostic {
            severity: mix_events::v1::Severity::Warning as i32,
            message: "Mix.".into(),
            ..mix_events::v1::Diagnostic::default()
        },
    )
    .unwrap();
    tree.finish(ROOT, Ending::succeeded().for_root(false))
        .unwrap();
    drop(tree);
    let (_, captured) = captured(&outbox.drain());

    let shown = rendered(&captured, Detail::Step);

    assert_eq!(
        shown,
        "warning: something went wrong inside `mix`\n\nhelp: report this bug at https://github.com/recregt/mix/issues\n\nCaused by:\n  Mix.\n"
    );
}
