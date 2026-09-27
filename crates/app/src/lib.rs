//! What `mix` does, in three layers.
//!
//! At the top, one module per command — [`bootstrap`], [`install`], [`remove`], [`doctor`], [`repair`].
//! A command decides what happens and in which order, and nothing else calls into it: the two
//! that need the same work done reach for the same layer below rather than for each other.
//!
//! Under them, the two things that work is done to: a user's [`profile`], which is rendered and
//! activated, and a declared [`target`], which is measured and reconciled. This is where the
//! logic that more than one command needs lives.
//!
//! At the bottom, the system surfaces a layer above talks to — a process ([`exec`]), a file
//! ([`fs`]), a unit ([`systemd`]), a repository ([`git`]), a [`mirror`] — each named for what it
//! talks to rather than for who happens to share it.

mod exec;
mod fs;
mod git;
mod mirror;
mod systemd;

pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod profile;
pub mod remove;
pub mod repair;
pub mod target;

pub use profile::resolve_existing_user_config;

#[doc(hidden)]
pub use exec::output;
