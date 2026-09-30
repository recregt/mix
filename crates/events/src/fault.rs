use crate::tree::Ending;
use crate::v1::{Cancellation, Code, Diagnostic};

#[derive(Debug, Clone, PartialEq)]
pub enum Fault {
    Failed(Diagnostic),
    Cancelled {
        cause: Cancellation,
        rolled_back: bool,
    },
}

impl Fault {
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
