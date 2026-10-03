use std::process::ExitCode;

use mix_events::Fault;
use mix_events::v1::Code;
use mix_events::v1::command::Request;
use mix_explain::Diagnostic;

use mix_render::words::{action, outcome};
use mix_render::{Format, View};

use crate::{client, request};

pub async fn run(request: Request, view: &View) -> ExitCode {
    let result = match request::refused(&request) {
        Some(fault) => Err(anyhow::Error::from(client::Failed {
            request: request.clone(),
            fault,
        })),
        None => client::run(request.clone(), request::route(&request), view).await,
    };
    if let Err(error) = &result
        && unstreamed(view)
    {
        stream_the_failure(&request, error, view);
    }
    let from_root = view
        .exit
        .code()
        .map(|code| ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX)));
    match result {
        Ok(()) => from_root.unwrap_or(ExitCode::SUCCESS),
        Err(error) => {
            if view.format == Format::Human && view.exit.code().is_none() {
                report(&error, &request, view.verbose);
            }
            from_root.unwrap_or(ExitCode::FAILURE)
        }
    }
}

fn unstreamed(view: &View) -> bool {
    view.streams() && !view.exit.started()
}

fn report(error: &anyhow::Error, request: &Request, verbose: u8) {
    let words = words(error, request);
    let fault = fault_of(error);
    let code = (verbose > 0)
        .then(|| fault.code())
        .flatten()
        .map(mix_explain::codes::kebab);
    let mut causes = mix_explain::evidence(&fault);
    for cause in mix_ui::causes_of(error.chain().nth(1), words.summary_text()) {
        if !causes.iter().any(|known| known.contains(&cause)) {
            causes.push(cause);
        }
    }
    mix_ui::report(
        mix_ui::Severity::Error,
        &words.report().code(code.as_deref()).causes(causes),
    );
}

fn words(error: &anyhow::Error, request: &Request) -> Diagnostic {
    if let Some(failed) = error.downcast_ref::<client::Failed>() {
        return outcome(&failed.request, &failed.fault);
    }
    if let Some(error) = error.downcast_ref::<mix_rpc::Error>() {
        return outcome(request, &rpc_fault(error));
    }
    mix_explain::failed(&*action(request))
}

fn fault_of(error: &anyhow::Error) -> Fault {
    if let Some(failed) = error.downcast_ref::<client::Failed>() {
        return failed.fault.clone();
    }
    if let Some(error) = error.downcast_ref::<mix_rpc::Error>() {
        return rpc_fault(error);
    }
    Fault::failed(Code::Internal, error.to_string(), None)
}

fn rpc_fault(error: &mix_rpc::Error) -> Fault {
    use mix_rpc::Error;

    let code = match error {
        Error::Spawn(_) | Error::Launch(_) | Error::Connect(_) | Error::Refused(_) => {
            Code::PrivilegesUnavailable
        }
        Error::Ended => Code::WorkerEnded,
        Error::VersionMismatch { .. } => Code::VersionMismatch,
        Error::Denied(_) => Code::NotBootstrapped,
        Error::Malformed(_) | Error::NotAConnection(_) => Code::Internal,
    };
    Fault::failed(code, error.to_string(), None)
}

fn stream_the_failure(request: &Request, error: &anyhow::Error, view: &View) {
    let Ok(mut sinks) = view.sinks(std::sync::Arc::new(mix_ui::Silent)) else {
        return;
    };
    mix_events::fail(
        uuid::Uuid::now_v7().to_string(),
        mix_events::command(request.clone()),
        fault_of(error),
        &mut sinks,
    );
}

#[cfg(test)]
mod tests {
    use mix_events::Diagnose;
    use mix_events::v1::envelope::Event;
    use mix_events::v1::{CleanRequest, InstallRequest, RemoveRequest, RepairRequest};

    use super::*;
    use mix_render::Exit;

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
            format: mix_render::Format::Human,
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
}
