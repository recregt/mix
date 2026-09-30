#[allow(clippy::all)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.serde.rs"));
}

mod fault;
mod outbox;
mod tree;
mod validate;

pub use fault::{Diagnose, Fault};
pub use outbox::Outbox;
pub use tree::{Ending, Misuse, Node, NodeId, ROOT, Start, Stopped, Tree, output};
pub use validate::{Entry, Outcome, Validated, Validator, Violation, validate};

pub const SCHEMA_MINOR: u32 = 0;
