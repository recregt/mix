#[allow(clippy::all)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.serde.rs"));
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.normalize.rs"));
}

pub trait Normalize {
    fn normalize(&mut self);
}

pub mod capture;
mod detail;
mod events {
    pub(crate) use crate::v1;
}
mod fault;
mod outbox;
mod tree;
mod validate;

pub use detail::{Detail, detail};
pub use fault::{Diagnose, Fault};
pub use outbox::Outbox;
pub use pbjson_types::Timestamp;
pub use tree::{Ending, Misuse, Node, NodeId, ROOT, Start, Stopped, Tree, exit, output};
pub use validate::{Entry, Outcome, Validated, Validator, Violation, validate};

pub const SCHEMA_MINOR: u32 = 0;
