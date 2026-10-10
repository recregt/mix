use std::sync::Arc;

use mix_events::v1::command::Request;
use mix_events::v1::node_started::Kind;
use mix_events::v1::{
    Action, Code, Command, InstallRequest, InstallResult, IoDetail, Plan, Rollback, Step, Verb,
};
use mix_events::{Ending, Outbox, Start, Tree};

use super::*;

fn command(dry_run: bool) -> Command {
    Command {
        dry_run,
        ..mix_events::command(Request::Install(InstallRequest {
            packages: vec!["hello".into()],
        }))
    }
}

fn step(subject: &str) -> Kind {
    Kind::Step(Step {
        verb: Verb::Writing as i32,
        subject: subject.into(),
    })
}

fn action(operation: Operation, subject: &str) -> Kind {
    Kind::Action(Action {
        operation: operation as i32,
        subject: subject.into(),
    })
}

fn io(path: &str) -> Diagnostic {
    Diagnostic {
        code: Code::PermissionDenied as i32,
        severity: Severity::Error as i32,
        message: format!("{path}: permission denied"),
        detail: Some(diagnostic::Detail::Io(IoDetail {
            path: path.into(),
            kind: "PermissionDenied".into(),
        })),
        ..Diagnostic::default()
    }
}

fn document_of(dry_run: bool, run: impl FnOnce(&mut Tree)) -> Document {
    let outbox = Arc::new(Outbox::new("r1", || {}));
    let mut tree = Tree::new(
        Arc::clone(&outbox),
        Arc::new(|| None),
        Start::command("install", command(dry_run)),
    );
    run(&mut tree);
    drop(tree);
    let mut builder = Builder::default();
    let documents: Vec<Document> = outbox
        .drain()
        .iter()
        .filter_map(|envelope| builder.envelope(envelope))
        .collect();
    let [document] = documents.as_slice() else {
        panic!("one document per request: {documents:?}");
    };
    document.clone()
}

#[test]
fn every_action_of_a_step_is_a_change_under_that_step() {
    let document = document_of(false, |tree| {
        let plan = tree
            .start(
                ROOT,
                Start::new(
                    "plan",
                    Kind::Plan(Plan {
                        title: "install".into(),
                    }),
                ),
            )
            .unwrap();
        let write = tree
            .start(plan, Start::new("write-config", step("package list")))
            .unwrap();
        for (operation, subject) in [
            (Operation::PutFile, "/home/alice/.local/state/mix/state"),
            (Operation::CreateDirs, "/home/alice/.local/state/mix"),
            (Operation::Commit, "changes"),
        ] {
            let node = tree
                .start(
                    write,
                    Start::new(subject.to_string(), action(operation, subject)),
                )
                .unwrap();
            tree.finish(node, Ending::succeeded()).unwrap();
        }
        tree.finish(write, Ending::succeeded()).unwrap();
        tree.finish(plan, Ending::succeeded()).unwrap();
        tree.finish(
            ROOT,
            Ending::succeeded().with_result(node_finished::Result::Install(InstallResult {
                added: vec!["hello".into()],
                ..InstallResult::default()
            })),
        )
        .unwrap();
    });

    assert_eq!(
        document,
        Document {
            format_version: "1.0".into(),
            request: "r1".into(),
            command: "install".into(),
            dry_run: false,
            status: Status::Succeeded as i32,
            exit: 0,
            result: Some(result::Result::Install(InstallResult {
                added: vec!["hello".into()],
                ..InstallResult::default()
            })),
            changes: vec![
                Change {
                    step: "write-config".into(),
                    action: ChangeAction::Update as i32,
                    operation: Operation::PutFile as i32,
                    subject: "/home/alice/.local/state/mix/state".into(),
                    status: Status::Succeeded as i32,
                    undone: false,
                    unknown: Vec::new(),
                },
                Change {
                    step: "write-config".into(),
                    action: ChangeAction::Create as i32,
                    operation: Operation::CreateDirs as i32,
                    subject: "/home/alice/.local/state/mix".into(),
                    status: Status::Succeeded as i32,
                    undone: false,
                    unknown: Vec::new(),
                },
            ],
            problems: Vec::new(),
            warnings: Vec::new(),
            cancellation: 0,
            steps: vec![StepResult {
                key: "write-config".into(),
                verb: Verb::Writing as i32,
                subject: "package list".into(),
                status: Status::Succeeded as i32,
                undone: false,
            }],
            waits: Vec::new(),
        }
    );
}

#[test]
fn a_rolled_back_step_marks_its_changes_undone_and_its_undo_is_no_change() {
    let document = document_of(false, |tree| {
        let write = tree
            .start(ROOT, Start::new("write-config", step("package list")))
            .unwrap();
        let put = tree
            .start(write, Start::new("a", action(Operation::PutFile, "/state")))
            .unwrap();
        tree.finish(put, Ending::succeeded()).unwrap();
        tree.finish(write, Ending::succeeded()).unwrap();
        let rollback = tree
            .start(
                ROOT,
                Start::new("undo", Kind::Rollback(Rollback { undoes: write })),
            )
            .unwrap();
        let restore = tree
            .start(
                rollback,
                Start::new("r", action(Operation::Restore, "/state")),
            )
            .unwrap();
        tree.finish(restore, Ending::succeeded()).unwrap();
        tree.finish(rollback, Ending::succeeded()).unwrap();
        tree.finish(ROOT, Ending::failed(io("/state"))).unwrap();
    });

    assert_eq!(document.changes.len(), 1);
    assert!(document.changes[0].undone);
    assert!(document.steps[0].undone);
    assert_eq!(document.status(), Status::Failed);
}

#[test]
fn a_failure_is_a_problem_with_its_code_title_subject_and_metadata() {
    let document = document_of(false, |tree| {
        tree.finish(ROOT, Ending::failed(io("/state"))).unwrap();
    });

    let [problem] = document.problems.as_slice() else {
        panic!("{:?}", document.problems);
    };
    assert_eq!(problem.r#type, "urn:mix:problem:permission-denied");
    assert_eq!(problem.code(), Code::PermissionDenied);
    assert_eq!(
        problem.title,
        "`mix` was not allowed to use a file or directory"
    );
    assert_eq!(problem.detail, "/state: permission denied");
    assert_eq!(problem.subject, "/state");
    assert!(!problem.help.is_empty());
    assert_eq!(
        problem.metadata,
        Some(problem::Metadata::Io(IoDetail {
            path: "/state".into(),
            kind: "PermissionDenied".into(),
        }))
    );
}

#[test]
fn a_warning_is_kept_apart_from_the_problems() {
    let document = document_of(false, |tree| {
        tree.warn(
            ROOT,
            Diagnostic {
                severity: Severity::Warning as i32,
                code: Code::GitRecordFailed as i32,
                message: "could not record the change in git".into(),
                ..Diagnostic::default()
            },
        )
        .unwrap();
        tree.finish(ROOT, Ending::succeeded()).unwrap();
    });

    assert!(document.problems.is_empty());
    assert_eq!(document.warnings[0].code(), Code::GitRecordFailed);
}

#[test]
fn a_dry_run_says_so() {
    let document = document_of(true, |tree| {
        tree.finish(ROOT, Ending::succeeded()).unwrap();
    });

    assert!(document.dry_run);
}

#[test]
fn the_document_is_written_with_the_canonical_json_names() {
    let document = document_of(true, |tree| {
        tree.finish(ROOT, Ending::failed(io("/state")).for_root(false))
            .unwrap();
    });

    let json = serde_json::to_value(&document).unwrap();

    assert_eq!(json["formatVersion"], "1.0");
    assert_eq!(json["dryRun"], true);
    assert_eq!(json["status"], "STATUS_FAILED");
    assert_eq!(json["exit"], 1);
    assert_eq!(json["problems"][0]["code"], "CODE_PERMISSION_DENIED");
    assert_eq!(json["problems"][0]["io"]["path"], "/state");
    assert_eq!(json["changes"], serde_json::json!([]));
    assert_eq!(json["warnings"], serde_json::json!([]));
}

#[test]
fn a_successful_run_still_states_its_exit_and_that_it_was_no_dry_run() {
    let document = document_of(false, |tree| {
        tree.finish(ROOT, Ending::succeeded().for_root(false))
            .unwrap();
    });

    let json = serde_json::to_value(&document).unwrap();

    assert_eq!(json["exit"], 0);
    assert_eq!(json["dryRun"], false);
    assert_eq!(json["problems"], serde_json::json!([]));
}

#[test]
fn a_command_line_mix_cannot_read_is_a_usage_document() {
    let document = usage("r1".into(), "unrecognized subcommand 'instal'");

    let json = serde_json::to_value(&document).unwrap();

    assert_eq!(json["status"], "STATUS_FAILED");
    assert_eq!(json["exit"], 2);
    assert_eq!(json["problems"][0]["code"], "CODE_USAGE");
    assert_eq!(
        json["problems"][0]["detail"],
        "unrecognized subcommand 'instal'"
    );
}

#[test]
fn every_wait_is_kept_with_what_held_it() {
    let lock = mix_events::v1::LockWait {
        lock: "/var/lib/mix/lock".into(),
        holder: Some("alice".into()),
        command: Some("install".into()),
    };
    let document = document_of(false, |tree| {
        let node = tree
            .start(ROOT, Start::new("lock", Kind::LockWait(lock.clone())))
            .unwrap();
        tree.finish(node, Ending::succeeded()).unwrap();
        tree.finish(ROOT, Ending::succeeded()).unwrap();
    });

    assert_eq!(document.waits, [lock]);
}

#[test]
fn a_target_repair_could_not_fix_is_a_problem_named_after_it() {
    let document = document_of(false, |tree| {
        tree.finish(
            ROOT,
            Ending::succeeded()
                .with_result(node_finished::Result::Repair(
                    mix_events::v1::RepairResult {
                        reports: vec![
                            mix_events::v1::RepairReport {
                                blocked_by: String::new(),
                                target: "/etc/nix".into(),
                                fixed: true,
                                failure: None,
                            },
                            mix_events::v1::RepairReport {
                                blocked_by: String::new(),
                                target: "mix-users".into(),
                                fixed: false,
                                failure: Some(Diagnostic {
                                    code: Code::Unspecified as i32,
                                    severity: Severity::Error as i32,
                                    message: "the group could not be read".into(),
                                    ..Diagnostic::default()
                                }),
                            },
                        ],
                    },
                ))
                .for_root(true),
        )
        .unwrap();
    });

    let subjects: Vec<&str> = document
        .problems
        .iter()
        .map(|problem| problem.subject.as_str())
        .collect();
    assert_eq!(subjects, ["mix-users"]);
}
