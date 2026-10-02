//! What `mix` does, split by what each part touches.
//!
//! [`ops`] holds one module per command. [`profile`] and [`target`] hold the work more than one
//! command needs: a user's profile, rendered and activated, and a declared target, measured and
//! reconciled. [`effect`] holds everything that reaches the system: accounts, files, processes,
//! git, systemd, the mirror and the lock.

mod context;

pub mod diagnose;
pub mod drive;

pub mod effect;
pub mod lock;
pub mod ops;
pub mod profile;
pub mod render;
pub mod request;
pub mod target;

pub use context::{Context, HostConfig, Request, request_id};
pub use request::{Caller, Session};

#[doc(hidden)]
pub use effect::exec::output;
