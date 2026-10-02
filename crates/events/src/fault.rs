#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use crate::tree::Ending;
use crate::v1::diagnostic::Detail;
use crate::v1::{Cancellation, Code, Diagnostic, Severity};

#[derive(Debug, Clone, PartialEq)]
pub enum Fault {
    Failed(Diagnostic),
    Cancelled {
        cause: Cancellation,
        rolled_back: bool,
    },
}

impl Fault {
    pub fn failed(code: Code, message: impl Into<String>, detail: Option<Detail>) -> Self {
        Fault::Failed(Diagnostic {
            code: code as i32,
            severity: Severity::Error as i32,
            node: 0,
            message: message.into(),
            causes: Vec::new(),
            detail,
        })
    }

    pub fn code(&self) -> Option<Code> {
        match self {
            Fault::Failed(diagnostic) => Some(diagnostic.code()),
            Fault::Cancelled { .. } => None,
        }
    }
}

impl From<Fault> for Ending {
    fn from(fault: Fault) -> Self {
        match fault {
            Fault::Failed(diagnostic) => Ending::failed(diagnostic),
            Fault::Cancelled { cause, .. } => Ending::cancelled(cause),
        }
    }
}

pub trait Diagnose {
    fn fault(&self) -> Fault;

    fn code(&self) -> Option<Code> {
        self.fault().code()
    }
}
