pub mod client;
pub mod controls;

use std::process::ExitCode;

use mix_events::Fault;
use mix_events::v1::Code;
use mix_events::v1::command::Request;
use mix_explain::Diagnostic;

use crate::args::Output;
use crate::output::View;
use crate::request;

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
            if view.output == Output::Human && view.exit.code().is_none() {
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

pub(crate) fn outcome(request: &Request, fault: &Fault) -> Diagnostic {
    mix_explain::outcome(request::name(request), &*request::action(request), fault)
}

fn words(error: &anyhow::Error, request: &Request) -> Diagnostic {
    if let Some(failed) = error.downcast_ref::<client::Failed>() {
        return outcome(&failed.request, &failed.fault);
    }
    if let Some(error) = error.downcast_ref::<mix_rpc::Error>() {
        return outcome(request, &rpc_fault(error));
    }
    mix_explain::failed(&*request::action(request))
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
mod tests;
