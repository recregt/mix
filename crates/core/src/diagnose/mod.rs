use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{
    Cancellation, Code, CommandDetail, Diagnostic, IoDetail, LockDetail, PackagesDetail, Severity,
    UnrepairableDetail,
};
use mix_events::{Diagnose, Fault};
use mix_nixlog::{NixFailure, nix_error};

use crate::Error;
use crate::action::Failure;
use crate::health::Unfixable;
use crate::plan::diagnostic;

fn command_code(tail: &str) -> Code {
    match nix_error(tail).and_then(|error| error.failure) {
        Some(NixFailure::Build { .. }) => Code::BuildFailed,
        Some(NixFailure::UnknownPackage { .. }) => Code::UnknownPackage,
        None => Code::CommandFailed,
    }
}

pub fn command_failure(
    command: &str,
    exit_status: Option<i32>,
    tail: &str,
    message: String,
) -> Diagnostic {
    let invocation = |output_tail: String| CommandDetail {
        command: command.to_string(),
        exit_status,
        output_tail,
    };
    let Some(error) = nix_error(tail) else {
        return Diagnostic {
            code: Code::CommandFailed as i32,
            severity: Severity::Error as i32,
            message,
            detail: Some(Detail::Command(invocation(tail.to_string()))),
            ..Diagnostic::default()
        };
    };
    let (code, package) = match error.failure {
        Some(NixFailure::Build { package }) => (Code::BuildFailed, package),
        Some(NixFailure::UnknownPackage { name }) => (Code::UnknownPackage, name),
        None => {
            return Diagnostic {
                code: Code::CommandFailed as i32,
                severity: Severity::Error as i32,
                message,
                detail: Some(Detail::Command(invocation(error.message))),
                ..Diagnostic::default()
            };
        }
    };
    Diagnostic {
        code: code as i32,
        severity: Severity::Error as i32,
        node: 0,
        message,
        causes: vec![Diagnostic {
            code: Code::CommandFailed as i32,
            severity: Severity::Error as i32,
            message: format!("`{command}` failed"),
            detail: Some(Detail::Command(invocation(error.message))),
            ..Diagnostic::default()
        }],
        detail: Some(Detail::Packages(PackagesDetail {
            packages: vec![package],
        })),
    }
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
        reason: crate::health::wire::unfixable(reason) as i32,
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
    Fault::failed(
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
            Error::Command { detail, .. } => command_code(detail),
            Error::Exec { .. } => Code::SpawnFailed,
            Error::Cancelled { .. } => return None,
            Error::LockMissing { .. } => Code::LockMissing,
        })
    }

    fn fault(&self) -> Fault {
        match self {
            Error::Io { path, source } => Fault::failed(
                if source.kind() == std::io::ErrorKind::PermissionDenied {
                    Code::PermissionDenied
                } else {
                    Code::Io
                },
                self.to_string(),
                Some(Detail::Io(IoDetail {
                    path: path.display().to_string(),
                    kind: mix_events::io_kind::name(source.kind()),
                })),
            ),
            Error::Command { command, detail } => {
                Fault::Failed(command_failure(command, None, detail, self.to_string()))
            }
            Error::Exec { command, .. } => Fault::failed(
                Code::SpawnFailed,
                self.to_string(),
                Some(Detail::Command(CommandDetail {
                    command: command.clone(),
                    exit_status: None,
                    output_tail: String::new(),
                })),
            ),
            Error::Cancelled { .. } => Fault::Cancelled {
                cause: Cancellation::Interrupted,
                rolled_back: false,
            },
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
