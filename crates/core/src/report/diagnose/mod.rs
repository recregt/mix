use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{
    Cancellation, Code, CommandDetail, ConflictDetail, Diagnostic, IntegrityDetail, IoDetail,
    LockDetail, NetworkDetail, PackagesDetail, Severity, UnitDetail, UnrepairableDetail,
};
use mix_events::{Diagnose, Fault};
use mix_nixlog::{NixFailure, nix_error};

use crate::Error;
use crate::effect::Failure;
use crate::ops::health::Unfixable;

fn command_code(tail: &str) -> Code {
    match nix_error(tail).and_then(|error| error.failure) {
        Some(NixFailure::Build { .. }) => Code::BuildFailed,
        Some(NixFailure::UnknownPackage { .. }) => Code::UnknownPackage,
        Some(NixFailure::DiskFull) => Code::DiskFull,
        Some(NixFailure::Download { .. }) => Code::Network,
        Some(NixFailure::Collision { .. }) => Code::PackageCollision,
        None => Code::CommandFailed,
    }
}

pub fn collision(packages: [&str; 2], file: &str, cause: Diagnostic) -> Diagnostic {
    Diagnostic {
        code: Code::PackageCollision as i32,
        severity: Severity::Error as i32,
        message: format!(
            "`{}` and `{}` both provide {file}",
            packages[0], packages[1]
        ),
        causes: vec![cause],
        detail: Some(Detail::Packages(PackagesDetail {
            packages: packages.map(str::to_string).to_vec(),
        })),
        ..Diagnostic::default()
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
    let cause = |output_tail: String| Diagnostic {
        code: Code::CommandFailed as i32,
        severity: Severity::Error as i32,
        message: format!("`{command}` failed"),
        detail: Some(Detail::Command(invocation(output_tail))),
        ..Diagnostic::default()
    };
    let (code, message, package) = match error.failure {
        Some(NixFailure::DiskFull) => {
            return Diagnostic {
                code: Code::DiskFull as i32,
                severity: Severity::Error as i32,
                message: "the disk that holds the Nix store is full".to_string(),
                causes: vec![cause(error.message)],
                ..Diagnostic::default()
            };
        }
        Some(NixFailure::Download { url }) => {
            return Diagnostic {
                code: Code::Network as i32,
                severity: Severity::Error as i32,
                message: format!("couldn't download {url}"),
                causes: vec![cause(error.message)],
                detail: Some(Detail::Network(NetworkDetail { url })),
                ..Diagnostic::default()
            };
        }
        Some(NixFailure::Collision { file, paths }) => {
            let names = paths.each_ref().map(|path| mix_nixlog::package_name(path));
            return collision(names, &file, cause(error.message));
        }
        Some(NixFailure::Build { package }) => (
            Code::BuildFailed,
            format!("couldn't build {package}"),
            package,
        ),
        Some(NixFailure::UnknownPackage { name }) => (
            Code::UnknownPackage,
            format!("`{name}` isn't a package `mix` can find"),
            name,
        ),
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
        causes: vec![cause(error.message)],
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
        reason: crate::report::inspection::unfixable(reason) as i32,
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
            Error::Effect(failure) => return failure.code(),
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
            Error::Effect(failure) => failure.fault(),
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

pub fn diagnostic(failure: &Failure) -> Diagnostic {
    let (code, message, detail) = match failure {
        Failure::Conflict {
            subject,
            expected,
            found,
        } => (
            Code::Conflict,
            format!("{subject}: expected {expected}, found {found}"),
            Some(Detail::Conflict(Box::new(ConflictDetail {
                subject: subject.clone(),
                expected: expected.clone(),
                found: found.clone(),
            }))),
        ),
        Failure::Io { path, kind } => (
            if *kind == std::io::ErrorKind::PermissionDenied {
                Code::PermissionDenied
            } else {
                Code::Io
            },
            format!("{}: {kind}", path.display()),
            Some(Detail::Io(IoDetail {
                path: path.display().to_string(),
                kind: format!("{kind:?}"),
            })),
        ),
        Failure::CommandFailed {
            program,
            status,
            output_tail,
        } => {
            return command_failure(program, *status, output_tail, format!("{program} failed"));
        }
        Failure::NoInodes {
            program,
            status,
            output_tail,
        } => {
            let full = command_failure(program, *status, output_tail, format!("{program} failed"));
            return Diagnostic {
                code: Code::NoInodes as i32,
                message: "the disk that holds the Nix store has no free inodes".to_string(),
                ..full
            };
        }
        Failure::OutOfMemory {
            program,
            status,
            output_tail,
        } => {
            let full = command_failure(program, *status, output_tail, format!("{program} failed"));
            return Diagnostic {
                code: Code::OutOfMemory as i32,
                message: "the build ran out of memory".to_string(),
                ..full
            };
        }
        Failure::Collision(collided) => {
            let program = &collided.program;
            let full = command_failure(
                program,
                collided.status,
                &collided.output_tail,
                format!("{program} failed"),
            );
            let cause = full.causes.into_iter().next().unwrap_or_default();
            return collision(
                collided.packages.each_ref().map(String::as_str),
                &collided.file,
                cause,
            );
        }
        Failure::SpawnFailed { program, kind } => {
            (Code::SpawnFailed, format!("{program}: {kind}"), None)
        }
        Failure::Unit(unit) => (
            Code::UnitFailed,
            format!(
                "could not {} {}: job {}, {} ({}), result {}",
                unit.operation.verb(),
                unit.unit,
                unit.job_result,
                unit.active_state,
                unit.sub_state,
                unit.unit_result
            ),
            Some(Detail::Unit(Box::new(UnitDetail {
                operation: unit.operation.verb().to_string(),
                unit: unit.unit.clone(),
                invocation: unit.invocation.clone(),
            }))),
        ),
        Failure::SystemdUnreachable => (
            Code::SystemdUnreachable,
            "systemd could not be reached".to_string(),
            None,
        ),
        Failure::Network { url } => (
            Code::Network,
            url.clone(),
            Some(Detail::Network(NetworkDetail { url: url.clone() })),
        ),
        Failure::Integrity {
            artifact,
            expected,
            found,
        } => (
            Code::Integrity,
            format!("{artifact}: expected {expected}, found {found}"),
            Some(Detail::Integrity(IntegrityDetail {
                artifact: artifact.clone(),
                expected: expected.clone(),
                actual: found.clone(),
            })),
        ),
        Failure::Cancelled => (Code::Internal, "cancelled".to_string(), None),
        Failure::Unrepairable { artifact, reason } => (
            Code::Unrepairable,
            format!("{artifact}: {reason}"),
            Some(Detail::Unrepairable(unrepairable(artifact, *reason))),
        ),
    };
    Diagnostic {
        code: code as i32,
        severity: Severity::Error as i32,
        node: 0,
        message,
        causes: Vec::new(),
        detail,
    }
}

#[cfg(test)]
mod tests;
