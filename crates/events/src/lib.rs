#[allow(clippy::all)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.serde.rs"));
}

mod tree;
mod validate;

pub use tree::{Ending, Node, ROOT, Sink, Stopped};
pub use validate::{Entry, Outcome, Validated, Validator, Violation, validate};

pub const SCHEMA_MINOR: u32 = 0;
