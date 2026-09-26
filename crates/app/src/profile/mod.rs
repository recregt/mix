//! The managed user profile: what it is declared to be, and how it is activated.
//!
//! A profile is a home-manager generation built from the flake and `home.nix` mix renders into
//! the user's state directory. Standing one up is `mix bootstrap`, changing one is
//! `mix install` or `mix remove`, and each of them does it through here.

mod activation;
pub mod change;
pub mod config;
pub mod state;

pub use activation::{BuildPolicy, activate, finish, switch};
pub use config::{
    existing_user_config_for, resolve_existing_user_config, resolve_user_config, user_config_for,
};

/// What an activation could not do.
///
/// Facts only: the derivations the binary cache had nothing for, or a process that failed. What
/// a reader should do about either depends on which command asked for the activation, so the
/// words are written in `mix-cli`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error("{}", refusal(packages.as_deref()))]
    SourceBuildRequired { packages: Option<Vec<String>> },
}

fn refusal(packages: Option<&[String]>) -> String {
    match packages {
        Some(packages) if !packages.is_empty() => {
            format!(
                "the binary cache has nothing to download for: {}",
                packages.join(", ")
            )
        }
        _ => "the binary cache cannot serve everything this build needs".to_string(),
    }
}

pub type Result<T> = std::result::Result<T, Error>;
