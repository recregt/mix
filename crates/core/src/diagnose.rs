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

pub fn warning(code: Code, message: impl Into<String>, cause: &dyn Diagnose) -> Diagnostic {
    let cause = match cause.fault() {
        Fault::Failed(diagnostic) => diagnostic,
        Fault::Cancelled { .. } => Diagnostic {
            code: Code::Unspecified as i32,
            severity: Severity::Error as i32,
            message: "interrupted".to_string(),
            ..Diagnostic::default()
        },
    };
    Diagnostic {
        code: code as i32,
        severity: Severity::Warning as i32,
        node: 0,
        message: message.into(),
        causes: vec![cause],
        detail: None,
    }
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

fn joined(parts: &[&str]) -> String {
    let mut text = String::with_capacity(parts.iter().map(|part| part.len()).sum());
    parts.iter().for_each(|part| text.push_str(part));
    text
}

fn lock(code: Code, path: &std::path::Path, message: impl FnOnce(&str) -> String) -> Fault {
    let path = match path.to_str() {
        Some(path) => std::borrow::Cow::Borrowed(path),
        None => path.to_string_lossy(),
    };
    failed(
        code,
        message(&path),
        Some(Detail::Lock(LockDetail {
            path: path.into_owned(),
            holder: None,
        })),
    )
}

impl Diagnose for Error {
    fn code(&self) -> Option<Code> {
        Some(match self {
            Error::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
                Code::PermissionDenied
            }
            Error::Io { .. } => Code::Io,
            Error::Command { .. } => Code::CommandFailed,
            Error::Exec { .. } => Code::SpawnFailed,
            Error::TaskPanicked(_) => Code::Internal,
            Error::Cancelled { .. } => return None,
            Error::Locked { .. } => Code::Locked,
            Error::LockMissing { .. } => Code::LockMissing,
        })
    }

    fn fault(&self) -> Fault {
        match self {
            Error::Io { path, source } => failed(
                if source.kind() == std::io::ErrorKind::PermissionDenied {
                    Code::PermissionDenied
                } else {
                    Code::Io
                },
                self.to_string(),
                Some(Detail::Io(IoDetail {
                    path: path.display().to_string(),
                    kind: format!("{:?}", source.kind()),
                })),
            ),
            Error::Command { command, detail } => failed(
                Code::CommandFailed,
                self.to_string(),
                Some(Detail::Command(CommandDetail {
                    command: command.clone(),
                    exit_status: None,
                    output_tail: detail.clone(),
                })),
            ),
            Error::Exec { command, .. } => failed(
                Code::SpawnFailed,
                self.to_string(),
                Some(Detail::Command(CommandDetail {
                    command: command.clone(),
                    exit_status: None,
                    output_tail: String::new(),
                })),
            ),
            Error::TaskPanicked(_) => failed(Code::Internal, self.to_string(), None),
            Error::Cancelled { .. } => Fault::Cancelled {
                cause: Cancellation::Interrupted,
                rolled_back: false,
            },
            Error::Locked { path } => lock(Code::Locked, path, |path| {
                joined(&["already locked: ", path])
            }),
            Error::LockMissing { path } => lock(Code::LockMissing, path, |path| {
                joined(&[
                    "the lock at ",
                    path,
                    " does not exist and cannot be created",
                ])
            }),
        }
    }
}

#[cfg(test)]
mod tests;
