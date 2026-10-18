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
            cancellation: None,
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
    assert!(json.get("cancellation").is_none(), "{json}");
}

#[test]
fn a_cancelled_run_states_what_cancelled_it() {
    let document = document_of(false, |tree| {
        tree.finish(
            ROOT,
            Ending::cancelled(mix_events::v1::Cancellation::Interrupted).for_root(false),
        )
        .unwrap();
    });

    let json = serde_json::to_value(&document).unwrap();

    assert_eq!(json["status"], "STATUS_CANCELLED");
    assert_eq!(json["cancellation"], "CANCELLATION_INTERRUPTED");
}

#[test]
fn a_result_writes_its_fields_even_at_their_default() {
    let document = document_of(false, |tree| {
        let result = mix_events::v1::InstallResult {
            added: vec!["jq".into()],
            ..mix_events::v1::InstallResult::default()
        };
        tree.finish(
            ROOT,
            Ending::succeeded()
                .with_result(mix_events::v1::node_finished::Result::Install(result))
                .for_root(false),
        )
        .unwrap();
    });

    let json = serde_json::to_value(&document).unwrap();

    assert_eq!(json["install"]["skipped"], serde_json::json!([]));
    assert_eq!(json["install"]["restored"], false);
}

#[test]
fn a_list_is_written_with_its_packages_outputs_and_64_bit_sizes_as_strings() {
    let document = document_of(false, |tree| {
        let result = mix_events::v1::ListResult {
            active: Some(mix_events::v1::ActiveGeneration {
                number: 12,
                store_path: "/nix/store/aaa-home-manager-generation".into(),
            }),
            packages: vec![mix_events::v1::InstalledPackage {
                name: "curl".into(),
                version: Some("8.22.0".into()),
                changelog: Some("https://curl.se/ch/8.22.0.html".into()),
                outputs: vec![mix_events::v1::PackageOutput {
                    output: "bin".into(),
                    store_path: "/nix/store/bbb-curl-8.22.0-bin".into(),
                    nar_size: 383_552,
                    built_locally: true,
                    signers: vec!["cache.nixos.org-1".into()],
                    ..mix_events::v1::PackageOutput::default()
                }],
                ..mix_events::v1::InstalledPackage::default()
            }],
        };
        tree.finish(
            ROOT,
            Ending::succeeded()
                .with_result(node_finished::Result::List(result))
                .for_root(false),
        )
        .unwrap();
    });

    let json = serde_json::to_value(&document).unwrap();

    assert_eq!(json["list"]["active"]["number"], "12");
    assert_eq!(
        json["list"]["active"]["storePath"],
        "/nix/store/aaa-home-manager-generation"
    );
    let curl = &json["list"]["packages"][0];
    assert_eq!(curl["name"], "curl");
    assert_eq!(curl["version"], "8.22.0");
    assert_eq!(curl["changelog"], "https://curl.se/ch/8.22.0.html");
    assert!(curl.get("pname").is_none(), "{curl}");
    assert_eq!(
        curl["outputs"][0]["storePath"],
        "/nix/store/bbb-curl-8.22.0-bin"
    );
    assert_eq!(curl["outputs"][0]["narSize"], "383552");
    assert_eq!(curl["outputs"][0]["builtLocally"], true);
    assert_eq!(curl["outputs"][0]["signers"][0], "cache.nixos.org-1");
}

#[test]
fn an_update_is_written_with_each_input_its_revisions_the_packages_and_the_generation() {
    let document = document_of(false, |tree| {
        let revision = |rev: &str, seconds: i64| mix_events::v1::LockedRevision {
            rev: rev.into(),
            nar_hash: "sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk=".into(),
            last_modified: Some(mix_events::Timestamp { seconds, nanos: 0 }),
        };
        let result = mix_events::v1::UpdateResult {
            inputs: vec![mix_events::v1::InputUpdate {
                name: "nixpkgs".into(),
                owner: "NixOS".into(),
                repo: "nixpkgs".into(),
                branch: "nixos-unstable".into(),
                outcome: mix_events::v1::InputOutcome::Updated as i32,
                from: Some(revision(
                    "efe6f071ede9d21c37462d2d6682d5e670099684",
                    1_789_426_278,
                )),
                to: Some(revision(
                    "6510408d147d8c5e6d76043e899e9251756f6a75",
                    1_791_594_108,
                )),
            }],
            packages: vec![
                mix_events::v1::PackageUpdate {
                    name: "qemu".into(),
                    from_version: Some("11.1.0".into()),
                    to_version: Some("11.1.1".into()),
                },
                mix_events::v1::PackageUpdate {
                    name: "newcomer".into(),
                    from_version: None,
                    to_version: Some("1.0".into()),
                },
            ],
            active: Some(mix_events::v1::ActiveGeneration {
                number: 7,
                store_path: "/nix/store/ccc-home-manager-generation".into(),
            }),
        };
        tree.finish(
            ROOT,
            Ending::succeeded()
                .with_result(node_finished::Result::Update(result))
                .for_root(false),
        )
        .unwrap();
    });

    let json = serde_json::to_value(&document).unwrap();

    let input = &json["update"]["inputs"][0];
    assert_eq!(input["name"], "nixpkgs");
    assert_eq!(input["owner"], "NixOS");
    assert_eq!(input["repo"], "nixpkgs");
    assert_eq!(input["branch"], "nixos-unstable");
    assert_eq!(input["outcome"], "INPUT_OUTCOME_UPDATED");
    assert_eq!(
        input["from"]["rev"],
        "efe6f071ede9d21c37462d2d6682d5e670099684"
    );
    assert_eq!(
        input["to"]["rev"],
        "6510408d147d8c5e6d76043e899e9251756f6a75"
    );
    assert_eq!(
        input["to"]["narHash"],
        "sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk="
    );
    assert_eq!(input["from"]["lastModified"], "2026-09-14T22:51:18+00:00");
    assert_eq!(input["to"]["lastModified"], "2026-10-10T01:01:48+00:00");
    let qemu = &json["update"]["packages"][0];
    assert_eq!(qemu["name"], "qemu");
    assert_eq!(qemu["fromVersion"], "11.1.0");
    assert_eq!(qemu["toVersion"], "11.1.1");
    let newcomer = &json["update"]["packages"][1];
    assert!(newcomer.get("fromVersion").is_none(), "{newcomer}");
    assert_eq!(newcomer["toVersion"], "1.0");
    assert_eq!(json["update"]["active"]["number"], "7");
    assert_eq!(
        json["update"]["active"]["storePath"],
        "/nix/store/ccc-home-manager-generation"
    );
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
