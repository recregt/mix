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
    fn fault(&self) -> Fault {
        let message = self.to_string();
        match self {
            BootstrapError::Core(error) => error.fault(),
            BootstrapError::Target(error) => error.fault(),
            BootstrapError::Interrupted => Fault::Cancelled {
                cause: Cancellation::Interrupted,
                rolled_back: true,
            },
            BootstrapError::Network(_) => failed(Code::Network, message, None),
            BootstrapError::Integrity { .. } => failed(Code::Integrity, message, None),
            BootstrapError::UnsupportedTarget(platform) => {
                failed(Code::UnsupportedTarget, message, target(platform))
            }
            BootstrapError::InvalidMirror(_) => failed(Code::InvalidMirror, message, None),
            BootstrapError::Conflict {
                subject,
                expected,
                found,
            } => failed(
                Code::Conflict,
                message,
                Some(Detail::Conflict(Box::new(ConflictDetail {
                    subject: subject.clone(),
                    expected: expected.clone(),
                    found: found.clone(),
                }))),
            ),
            BootstrapError::Decompression(_) => failed(Code::Decompression, message, None),
            BootstrapError::MalformedArchive(_) => failed(Code::MalformedArchive, message, None),
            BootstrapError::NotRoot(_) => failed(Code::NotRoot, message, None),
            BootstrapError::UnsupportedHost => {
                failed(Code::UnsupportedHost, message, host(WireHost::Nixos, ""))
            }
            BootstrapError::UnsupportedKernel => failed(
                Code::UnsupportedKernel,
                message,
                host(WireHost::Wsl, "wsl1"),
            ),
            BootstrapError::SystemdNotReady { host: found } => failed(
                Code::SystemdNotReady,
                message,
                host(
                    match found {
                        Host::Native => WireHost::Native,
                        Host::Wsl => WireHost::Wsl,
                    },
                    "",
                ),
            ),
            BootstrapError::SystemdUnreachable => failed(Code::SystemdUnreachable, message, None),
            BootstrapError::Unit {
                operation,
                unit,
                invocation,
                ..
            } => failed(
                Code::UnitFailed,
                message,
                Some(Detail::Unit(Box::new(UnitDetail {
                    operation: operation.clone(),
                    unit: unit.clone(),
                    invocation: invocation.clone(),
                }))),
            ),
            BootstrapError::AlreadyManaged => failed(Code::AlreadyManaged, message, None),
            BootstrapError::CrossDeviceStore { path } => failed(
                Code::CrossDeviceStore,
                message,
                Some(Detail::Path(PathDetail {
                    path: path.display().to_string(),
                })),
            ),
            BootstrapError::Rollback { cause, .. } => {
                let mut fault = failed(Code::RollbackIncomplete, message, None);
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
    fn fault(&self) -> Fault {
        let message = self.to_string();
        match self {
            ChangeError::Core(error) => error.fault(),
            ChangeError::InvalidPackage(error) => match error.rejected() {
                Some(name) => failed(Code::InvalidPackage, message, packages([name])),
                None => failed(Code::Internal, message, None),
            },
            ChangeError::InvalidState(Invalid::Package(name)) => {
                failed(Code::InvalidState, message, packages([name.as_str()]))
            }
            ChangeError::InvalidState(_) => failed(Code::InvalidState, message, None),
            ChangeError::NewerState(format) => failed(
                Code::NewerState,
                message,
                Some(Detail::Format(FormatDetail { format: *format })),
            ),
            ChangeError::NotRoot => failed(Code::RootNotAllowed, message, None),
            ChangeError::NotBootstrapped => failed(Code::NotBootstrapped, message, None),
        }
    }
}

impl Diagnose for RemoveError {
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
