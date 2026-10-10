use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Action, error_kind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Performed {
    pub undo: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Failure {
    Conflict {
        subject: String,
        expected: String,
        found: String,
    },
    Io {
        path: PathBuf,
        #[serde(with = "error_kind")]
        kind: std::io::ErrorKind,
    },
    CommandFailed {
        program: String,
        status: Option<i32>,
        output_tail: String,
    },
    SpawnFailed {
        program: String,
        #[serde(with = "error_kind")]
        kind: std::io::ErrorKind,
    },
    Unit(Box<UnitFailure>),
    SystemdUnreachable,
    Network {
        url: String,
    },
    Integrity {
        artifact: String,
        expected: String,
        found: String,
    },
    Cancelled,
    Unrepairable {
        artifact: String,
        reason: crate::ops::health::Unfixable,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitOperation {
    Inspect,
    Reload,
    Enable,
    Disable,
    Start,
    Stop,
    Restart,
}

impl UnitOperation {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Reload => "reload",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitFailure {
    pub operation: UnitOperation,
    pub unit: String,
    pub job_result: String,
    pub active_state: String,
    pub sub_state: String,
    pub unit_result: String,
    pub invocation: Option<String>,
}

pub type Outcome = Result<Performed, Failure>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_io_error_kind_survives_serialization() {
        for kind in mix_events::io_kind::KINDS {
            let failure = Failure::Io {
                path: "/home/alice".into(),
                kind: *kind,
            };
            let json = serde_json::to_string(&failure).unwrap();

            assert_eq!(serde_json::from_str::<Failure>(&json).unwrap(), failure);
        }
    }
}
