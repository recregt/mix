use mix_core::change::Invalid;
use mix_core::diagnose::{failed, unrepairable};
use mix_events::v1::diagnostic::Detail;
use mix_events::v1::{
    Cancellation, Code, ConflictDetail, FormatDetail, Host as WireHost, HostDetail, PackagesDetail,
    PathDetail, TargetDetail, UnitDetail,
};
use mix_events::{Diagnose, Fault};

use crate::ops::bootstrap::{Error as BootstrapError, Host};
use crate::ops::remove::Error as RemoveError;
use crate::profile::change::Error as ChangeError;
use crate::target::Error as TargetError;

fn packages(names: impl IntoIterator<Item = impl Into<String>>) -> Option<Detail> {
    Some(Detail::Packages(PackagesDetail {
        packages: names.into_iter().map(Into::into).collect(),
    }))
}

fn host(host: WireHost, state: &str) -> Option<Detail> {
    Some(Detail::Host(HostDetail {
        host: host as i32,
        state: state.to_string(),
    }))
}

fn target(platform: &str) -> Option<Detail> {
    let (arch, os) = platform.split_once('-').unwrap_or((platform, ""));
    Some(Detail::Target(TargetDetail {
        arch: arch.to_string(),
        os: os.to_string(),
    }))
}

impl Diagnose for BootstrapError {
    fn code(&self) -> Option<Code> {
        Some(match self {
            BootstrapError::Core(error) => return error.code(),
            BootstrapError::Target(error) => return error.code(),
            BootstrapError::Interrupted => return None,
            BootstrapError::Network(_) => Code::Network,
            BootstrapError::Integrity { .. } => Code::Integrity,
            BootstrapError::UnsupportedTarget(_) => Code::UnsupportedTarget,
            BootstrapError::InvalidMirror(_) => Code::InvalidMirror,
            BootstrapError::Conflict { .. } => Code::Conflict,
            BootstrapError::Decompression(_) => Code::Decompression,
            BootstrapError::MalformedArchive(_) => Code::MalformedArchive,
            BootstrapError::NotRoot(_) => Code::NotRoot,
            BootstrapError::UnsupportedHost => Code::UnsupportedHost,
            BootstrapError::UnsupportedKernel => Code::UnsupportedKernel,
            BootstrapError::SystemdNotReady { .. } => Code::SystemdNotReady,
            BootstrapError::SystemdUnreachable => Code::SystemdUnreachable,
            BootstrapError::Unit { .. } => Code::UnitFailed,
            BootstrapError::AlreadyManaged => Code::AlreadyManaged,
            BootstrapError::CrossDeviceStore { .. } => Code::CrossDeviceStore,
            BootstrapError::Rollback { .. } => Code::RollbackIncomplete,
        })
    }

    fn fault(&self) -> Fault {
        match self {
            BootstrapError::Core(error) => error.fault(),
            BootstrapError::Target(error) => error.fault(),
            BootstrapError::Interrupted => Fault::Cancelled {
                cause: Cancellation::Interrupted,
                rolled_back: true,
            },
            BootstrapError::Network(_) => failed(Code::Network, self.to_string(), None),
            BootstrapError::Integrity { .. } => failed(Code::Integrity, self.to_string(), None),
            BootstrapError::UnsupportedTarget(platform) => {
                failed(Code::UnsupportedTarget, self.to_string(), target(platform))
            }
            BootstrapError::InvalidMirror(_) => failed(Code::InvalidMirror, self.to_string(), None),
            BootstrapError::Conflict {
                subject,
                expected,
                found,
            } => failed(
                Code::Conflict,
                self.to_string(),
                Some(Detail::Conflict(Box::new(ConflictDetail {
                    subject: subject.clone(),
                    expected: expected.clone(),
                    found: found.clone(),
                }))),
            ),
            BootstrapError::Decompression(_) => failed(Code::Decompression, self.to_string(), None),
            BootstrapError::MalformedArchive(_) => {
                failed(Code::MalformedArchive, self.to_string(), None)
            }
            BootstrapError::NotRoot(_) => failed(Code::NotRoot, self.to_string(), None),
            BootstrapError::UnsupportedHost => failed(
                Code::UnsupportedHost,
                self.to_string(),
                host(WireHost::Nixos, ""),
            ),
            BootstrapError::UnsupportedKernel => failed(
                Code::UnsupportedKernel,
                self.to_string(),
                host(WireHost::Wsl, "wsl1"),
            ),
            BootstrapError::SystemdNotReady { host: found } => failed(
                Code::SystemdNotReady,
                self.to_string(),
                host(
                    match found {
                        Host::Native => WireHost::Native,
                        Host::Wsl => WireHost::Wsl,
                    },
                    "",
                ),
            ),
            BootstrapError::SystemdUnreachable => {
                failed(Code::SystemdUnreachable, self.to_string(), None)
            }
            BootstrapError::Unit {
                operation,
                unit,
                invocation,
                ..
            } => failed(
                Code::UnitFailed,
                self.to_string(),
                Some(Detail::Unit(Box::new(UnitDetail {
                    operation: operation.clone(),
                    unit: unit.clone(),
                    invocation: invocation.clone(),
                }))),
            ),
            BootstrapError::AlreadyManaged => failed(Code::AlreadyManaged, self.to_string(), None),
            BootstrapError::CrossDeviceStore { path } => failed(
                Code::CrossDeviceStore,
                self.to_string(),
                Some(Detail::Path(PathDetail {
                    path: path.display().to_string(),
                })),
            ),
            BootstrapError::Rollback { cause, .. } => {
                let mut fault = failed(Code::RollbackIncomplete, self.to_string(), None);
                if let (Fault::Failed(diagnostic), Fault::Failed(cause)) =
                    (&mut fault, cause.fault())
                {
                    diagnostic.causes.push(cause);
                }
                fault
            }
        }
    }
}

impl Diagnose for ChangeError {
    fn code(&self) -> Option<Code> {
        Some(match self {
            ChangeError::Core(error) => return error.code(),
            ChangeError::InvalidPackage(error) if error.rejected().is_some() => {
                Code::InvalidPackage
            }
            ChangeError::InvalidPackage(_) => Code::Internal,
            ChangeError::InvalidState(_) => Code::InvalidState,
            ChangeError::NewerState(_) => Code::NewerState,
            ChangeError::NotRoot => Code::RootNotAllowed,
            ChangeError::NotBootstrapped => Code::NotBootstrapped,
        })
    }

    fn fault(&self) -> Fault {
        match self {
            ChangeError::Core(error) => error.fault(),
            ChangeError::InvalidPackage(error) => match error.rejected() {
                Some(name) => failed(Code::InvalidPackage, self.to_string(), packages([name])),
                None => failed(Code::Internal, self.to_string(), None),
            },
            ChangeError::InvalidState(Invalid::Package(name)) => failed(
                Code::InvalidState,
                self.to_string(),
                packages([name.as_str()]),
            ),
            ChangeError::InvalidState(_) => failed(Code::InvalidState, self.to_string(), None),
            ChangeError::NewerState(format) => failed(
                Code::NewerState,
                self.to_string(),
                Some(Detail::Format(FormatDetail { format: *format })),
            ),
            ChangeError::NotRoot => failed(Code::RootNotAllowed, self.to_string(), None),
            ChangeError::NotBootstrapped => failed(Code::NotBootstrapped, self.to_string(), None),
        }
    }
}

impl Diagnose for RemoveError {
    fn code(&self) -> Option<Code> {
        match self {
            RemoveError::Change(error) => error.code(),
            RemoveError::Protected(_) => Some(Code::ProtectedPackage),
        }
    }

    fn fault(&self) -> Fault {
        match self {
            RemoveError::Change(error) => error.fault(),
            RemoveError::Protected(names) => failed(
                Code::ProtectedPackage,
                self.to_string(),
                packages(names.iter().map(String::as_str)),
            ),
        }
    }
}

impl Diagnose for TargetError {
    fn code(&self) -> Option<Code> {
        match self {
            TargetError::Core(error) => error.code(),
            TargetError::Unrepairable { .. } => Some(Code::Unrepairable),
        }
    }

    fn fault(&self) -> Fault {
        match self {
            TargetError::Core(error) => error.fault(),
            TargetError::Unrepairable { artifact, reason } => failed(
                Code::Unrepairable,
                self.to_string(),
                Some(Detail::Unrepairable(unrepairable(artifact, *reason))),
            ),
        }
    }
}

#[cfg(test)]
mod tests;
