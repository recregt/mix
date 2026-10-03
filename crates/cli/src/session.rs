use std::process::ExitCode;

use mix_events::v1::command::Request;
use mix_render::View;

use crate::client::{self, Failure};
use crate::request;

pub async fn run(request: Request, view: &View) -> ExitCode {
    let result = match request::refused(&request) {
        Some(fault) => Err(Failure::Failed(Box::new(fault))),
        None => client::run(&request, request::route(&request), view).await,
    };
    if let Err(failure) = &result {
        view.failed(
            uuid::Uuid::now_v7().to_string(),
            &request,
            &failure.fault(),
            failure.source(),
        );
    }
    let from_root = view
        .exit
        .code()
        .map(|code| ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX)));
    match result {
        Ok(()) => from_root.unwrap_or(ExitCode::SUCCESS),
        Err(_) => from_root.unwrap_or(ExitCode::FAILURE),
    }
}

#[cfg(test)]
mod tests {
    use mix_events::Fault;
    use mix_events::v1::{
        CleanRequest, Code, InstallRequest, RemoveRequest, RepairRequest, command::Request,
    };
    use mix_render::words::outcome;

    use crate::client::Failure;

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

    fn says(request: &Request, failure: Failure) -> String {
        outcome(request, &failure.fault()).message()
    }

    #[test]
    fn a_refused_sudo_is_about_administrator_rights() {
        assert_eq!(
            says(
                &repair(),
                Failure::Transport(mix_rpc::Error::Refused("connection closed".into()))
            ),
            "couldn't get administrator rights to finish the repair\n\
             make sure your account can use sudo, then try again"
        );
    }

    #[test]
    fn a_worker_that_stopped_mid_way_says_to_run_the_command_again() {
        let message = says(&repair(), Failure::Transport(mix_rpc::Error::Ended));

        assert!(message.contains("stopped before it could finish the repair"));
        assert!(message.contains("run the same command again"));
    }

    #[test]
    fn an_events_file_that_cannot_be_written_says_what_could_not_be_done() {
        for (request, words_for) in [
            (install(&["x"]), "couldn't install x"),
            (remove(&["git"]), "couldn't remove git"),
            (repair(), "couldn't finish the repair"),
            (clean(), "couldn't clean up your profile"),
        ] {
            let failure = Failure::Output(std::io::ErrorKind::PermissionDenied.into());
            assert_eq!(failure.fault().code(), Some(Code::Io));
            assert_eq!(
                says(&request, failure),
                format!("{words_for}\nrun it again with `-v` to see what went wrong")
            );
        }
    }

    #[test]
    fn a_failure_from_the_daemon_is_worded_for_the_request_it_ended() {
        let fault = Fault::failed(Code::RootNotAllowed, "root", None);

        assert!(
            says(&install(&["x"]), Failure::Failed(Box::new(fault)))
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
        for error in errors {
            let expected = match &error {
                Error::Malformed(_) | Error::NotAConnection(_) => vec![Code::Internal],
                Error::VersionMismatch { .. } => vec![Code::VersionMismatch],
                Error::Denied(_) => vec![Code::NotBootstrapped],
                Error::Spawn(_)
                | Error::Launch(_)
                | Error::Connect(_)
                | Error::Refused(_)
                | Error::Ended => vec![Code::PrivilegesUnavailable, Code::WorkerEnded],
            };
            let code = Failure::Transport(error).fault().code();
            assert!(
                code.is_some_and(|code| expected.contains(&code)),
                "{code:?}"
            );
        }
    }
}
