use mix_events::Diagnose;
use mix_events::v1::envelope::Event;
use mix_events::v1::{CleanRequest, InstallRequest, RemoveRequest, RepairRequest};

use super::*;
use crate::output::Exit;

fn install(packages: &[&str]) -> Request {
    Request::Install(InstallRequest {
        packages: packages.iter().map(|name| name.to_string()).collect(),
    })
}

fn remove(packages: &[&str]) -> Request {
    Request::Remove(RemoveRequest {
        packages: packages.iter().map(|name| name.to_string()).collect(),
    })
}

fn repair() -> Request {
    Request::Repair(RepairRequest {})
}

fn clean() -> Request {
    Request::Clean(CleanRequest { all: false })
}

fn recording(file: std::path::PathBuf) -> View {
    View {
        output: Output::Human,
        events_file: Some(file),
        verbose: 0,
        quiet: false,
        exit: Exit::default(),
    }
}

#[test]
fn a_refused_sudo_is_about_administrator_rights() {
    let error = anyhow::Error::from(mix_rpc::Error::Refused("connection closed".into()));

    assert_eq!(
        words(&error, &repair()).message(),
        "couldn't get administrator rights to finish the repair\n\
         make sure your account can use sudo, then try again"
    );
}

#[test]
fn a_worker_that_stopped_mid_way_says_to_run_the_command_again() {
    let message = words(&anyhow::Error::from(mix_rpc::Error::Ended), &repair()).message();

    assert!(message.contains("stopped before it could finish the repair"));
    assert!(message.contains("run the same command again"));
}

#[test]
fn an_error_from_the_client_itself_says_what_could_not_be_done() {
    let error = anyhow::anyhow!("something else broke");

    for (request, words_for) in [
        (install(&["x"]), "couldn't install x"),
        (remove(&["git"]), "couldn't remove git"),
        (repair(), "couldn't finish the repair"),
        (clean(), "couldn't clean up your profile"),
    ] {
        assert_eq!(
            words(&error, &request).message(),
            format!("{words_for}\nrun it again with `-v` to see what went wrong")
        );
    }
}

#[test]
fn a_failure_from_the_daemon_is_worded_for_the_request_it_ended() {
    let failed = client::Failed {
        request: install(&["x"]),
        fault: mix_shell::profile::change::Error::NotRoot.fault(),
    };

    assert!(
        words(&anyhow::Error::from(failed), &repair())
            .message()
            .contains("`mix install` can't be run as root")
    );
}

#[test]
fn every_worker_failure_has_a_code_unless_it_is_a_protocol_violation() {
    use mix_rpc::Error;

    let errors = vec![
        Error::Spawn(std::io::ErrorKind::NotFound.into()),
        Error::Connect("refused".into()),
        Error::Refused("not allowed".into()),
        Error::Ended,
        Error::VersionMismatch {
            ours: "1.0.0".into(),
            theirs: "1.1.0".into(),
        },
        Error::Denied("uid 1001 is not root and not a member of mix-users".into()),
        Error::NotAConnection(std::io::ErrorKind::InvalidInput.into()),
    ];
    for error in &errors {
        let code = rpc_fault(error).code();
        match error {
            Error::Malformed(_) | Error::NotAConnection(_) => {
                assert_eq!(code, Some(Code::Internal))
            }
            Error::VersionMismatch { .. } => assert_eq!(code, Some(Code::VersionMismatch)),
            Error::Denied(_) => assert_eq!(code, Some(Code::NotBootstrapped)),
            Error::Spawn(_)
            | Error::Launch(_)
            | Error::Connect(_)
            | Error::Refused(_)
            | Error::Ended => assert!(
                matches!(code, Some(Code::PrivilegesUnavailable | Code::WorkerEnded)),
                "{error:?}: {code:?}"
            ),
        }
    }
}

#[test]
fn a_stream_that_already_started_is_left_as_it_ended_rather_than_given_a_second_root() {
    let directory = tempfile::tempdir().unwrap();
    let view = recording(directory.path().join("events.ndjson"));
    assert!(unstreamed(&view));

    let mut sinks = view.sinks(std::sync::Arc::new(mix_ui::Silent)).unwrap();
    let outbox = std::sync::Arc::new(mix_events::Outbox::new("request", || {}));
    let tree = mix_events::Tree::new(
        std::sync::Arc::clone(&outbox),
        std::sync::Arc::new(|| None),
        mix_events::Start::command("repair", mix_events::v1::Command::default()),
    );
    for envelope in outbox.drain() {
        mix_events::Render::envelope(&mut sinks, envelope);
    }

    assert!(!unstreamed(&view));
    assert_eq!(view.exit.code(), None);
    drop(tree);
}

#[test]
fn a_failure_before_any_work_still_records_a_valid_stream_with_its_code() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("events.ndjson");
    let view = recording(file.clone());
    let request = install(&["ripgrep"]);
    let refused = anyhow::Error::from(client::Failed {
        request: request.clone(),
        fault: mix_shell::profile::change::Error::NotRoot.fault(),
    });

    stream_the_failure(&request, &refused, &view);

    let captured =
        mix_events::capture::read(std::io::BufReader::new(std::fs::File::open(&file).unwrap()))
            .unwrap();
    mix_events::validate(captured.envelopes.iter()).unwrap();
    let root = captured
        .envelopes
        .iter()
        .find_map(|envelope| match &envelope.event {
            Some(Event::NodeFinished(finished)) if finished.id == mix_events::ROOT => {
                Some(finished.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(root.diagnostic.unwrap().code(), Code::RootNotAllowed);
    assert_eq!(root.exit_code, mix_events::exit::FAILED);
    assert_eq!(view.exit.code(), Some(mix_events::exit::FAILED));
}
