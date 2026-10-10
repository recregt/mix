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
pub mod code;
mod detail;
mod events {
    pub(crate) use crate::v1;
}
mod fault;
pub mod io_kind;
pub mod mirror;
mod outbox;
pub mod result {
    #[allow(clippy::all)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/mix.result.v1.rs"));
        include!(concat!(env!("OUT_DIR"), "/mix.result.v1.serde.rs"));
    }

    pub const FORMAT_VERSION: &str = "1.0";
}
mod root;
mod tree;
mod validate;

pub use detail::{Detail, detail};
pub use fault::{Diagnose, Fault};
pub use outbox::Outbox;
pub use pbjson_types::Timestamp;
pub use root::{Render, command, fail, key_of};
pub use tree::{Ending, Misuse, Node, NodeId, ROOT, Start, Stopped, Tree, exit, output};
pub use validate::{Entry, Outcome, Validated, Validator, Violation, validate};

pub const SCHEMA_MINOR: u32 = 9;
