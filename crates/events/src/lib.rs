#[allow(clippy::all)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/mix.events.v1.serde.rs"));
}

mod stamp;
mod tree;

pub use stamp::{Clock, Stamper, SystemClock};
pub use tree::{Ending, Node, ROOT, Sink, Stopped};

pub const SCHEMA_MINOR: u32 = 0;
