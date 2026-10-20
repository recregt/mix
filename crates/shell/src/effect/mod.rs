pub mod accounts;
pub(crate) mod exec;
pub(crate) mod fs;
pub mod generations;
pub(crate) mod git;
pub mod identity;
pub mod installed;
pub mod journal;
pub(crate) mod mirror;
pub(crate) mod program;
pub(crate) mod removal;
pub mod runtime;
pub mod tools;
pub mod units;

use std::path::Path;

use mix_core::effect::Failure;

pub fn core_error(failure: Failure, path: &Path) -> mix_core::Error {
    match failure {
        Failure::Io { path, kind } => mix_core::Error::Io {
            path,
            source: kind.into(),
        },
        Failure::SpawnFailed { program, kind } => mix_core::Error::Exec {
            command: program,
            source: kind.into(),
        },
        Failure::Cancelled => mix_core::Error::Cancelled {
            command: format!("change {}", path.display()),
        },
        other => mix_core::Error::Effect(Box::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use mix_events::Diagnose;
    use mix_events::v1::Code;

    use super::*;

    #[test]
    fn a_failed_command_keeps_its_own_diagnosis() {
        let failure = Failure::CommandFailed {
            program: "git hash-object -- state".into(),
            status: Some(128),
            output_tail: "fatal: could not open 'state' for reading: Permission denied".into(),
        };

        let error = core_error(failure.clone(), Path::new("/home/alice/.local/state/mix"));

        assert_eq!(error.code(), Some(Code::CommandFailed));
        assert_eq!(error.fault(), failure.fault());
    }

    #[test]
    fn a_denied_write_stays_a_permission_error_with_its_path() {
        let error = core_error(
            Failure::Io {
                path: "/home/alice/.local/state/mix/state".into(),
                kind: std::io::ErrorKind::PermissionDenied,
            },
            Path::new("/home/alice/.local/state/mix"),
        );

        assert_eq!(error.code(), Some(Code::PermissionDenied));
        assert!(matches!(
            error,
            mix_core::Error::Io { path, .. } if path == Path::new("/home/alice/.local/state/mix/state")
        ));
    }
}
