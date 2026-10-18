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
        other => mix_core::Error::Command {
            command: format!("change {}", path.display()),
            detail: format!("{other:?}"),
        },
    }
}
