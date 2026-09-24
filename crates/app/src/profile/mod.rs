//! The managed user profile: what it is declared to be, and how it is activated.
//!
//! A profile is a home-manager generation built from the flake and `home.nix` mix renders into
//! the user's state directory. Standing one up is `mix bootstrap`, changing one is
//! `mix install` or `mix remove`, and each of them does it through here.

mod activation;
pub mod change;
pub mod config;

pub use activation::{BuildPolicy, activate};
pub use config::{resolve_existing_user_config, resolve_user_config};

/// What an activation could not do.
///
/// Facts only: the derivations the binary cache had nothing for, or a process that failed. What
/// a reader should do about either depends on which command asked for the activation, so the
/// words are written in `mix-cli`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error("the binary cache has nothing to download for: {}", .0.join(", "))]
    SourceBuildRequired(Vec<String>),
}

pub type Result<T> = std::result::Result<T, Error>;
