use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{
    Cancellation, Code, CommandDetail, Diagnostic, IoDetail, LockDetail, Severity,
    Unfixable as WireUnfixable, UnrepairableDetail,
};
use mix_events::{Diagnose, Fault};

use crate::Error;
use crate::action::Failure;
use crate::health::Unfixable;
use crate::plan::diagnostic;

pub fn failed(code: Code, message: impl Into<String>, detail: Option<Detail>) -> Fault {
    Fault::Failed(Diagnostic {
        code: code as i32,
        severity: Severity::Error as i32,
        node: 0,
        message: message.into(),
        causes: Vec::new(),
        detail,
    })
}

pub fn unrepairable(artifact: &str, reason: Unfixable) -> UnrepairableDetail {
    UnrepairableDetail {
        artifact: artifact.to_string(),
        reason: match reason {
            Unfixable::NotADirectory => WireUnfixable::NotADirectory,
            Unfixable::MissingUser => WireUnfixable::MissingUser,
            Unfixable::MissingRuntime => WireUnfixable::MissingRuntime,
        } as i32,
    }
}

impl Diagnose for Failure {
    fn fault(&self) -> Fault {
        match self {
            Failure::Cancelled => Fault::Cancelled {
                cause: Cancellation::Interrupted,
                rolled_back: false,
            },
            failure => Fault::Failed(diagnostic(failure)),
        }
    }
}

impl Diagnose for Error {
    fn fault(&self) -> Fault {
        let message = self.to_string();
        match self {
            Error::Io { path, source } => failed(
                if source.kind() == std::io::ErrorKind::PermissionDenied {
                    Code::PermissionDenied
                } else {
                    Code::Io
                },
                message,
                Some(Detail::Io(IoDetail {
                    path: path.display().to_string(),
                    kind: format!("{:?}", source.kind()),
                })),
            ),
            Error::Command { command, detail } => failed(
                Code::CommandFailed,
                message,
                Some(Detail::Command(CommandDetail {
                    command: command.clone(),
                    exit_status: None,
                    output_tail: detail.clone(),
                })),
            ),
            Error::Exec { command, .. } => failed(
                Code::SpawnFailed,
                message,
                Some(Detail::Command(CommandDetail {
                    command: command.clone(),
                    exit_status: None,
                    output_tail: String::new(),
                })),
            ),
            Error::TaskPanicked(_) => failed(Code::Internal, message, None),
            Error::Cancelled { .. } => Fault::Cancelled {
                cause: Cancellation::Interrupted,
                rolled_back: false,
            },
            Error::Locked { path } => failed(
                Code::Locked,
                message,
                Some(Detail::Lock(LockDetail {
                    path: path.display().to_string(),
                    holder: None,
                })),
            ),
            Error::LockMissing { path } => failed(
                Code::LockMissing,
                message,
                Some(Detail::Lock(LockDetail {
                    path: path.display().to_string(),
                    holder: None,
                })),
            ),
        }
    }
}

#[cfg(test)]
mod tests;
