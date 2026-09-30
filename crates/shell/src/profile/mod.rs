//! The managed user profile: what it is declared to be, and how it is activated.
//!
//! A profile is a home-manager generation built from the flake and `home.nix` mix renders into
//! the user's state directory. Standing one up is `mix bootstrap`, changing one is
//! `mix install` or `mix remove`, and each of them does it through here.

mod activation;
pub mod change;
pub mod config;
pub mod state;

pub use activation::{activate_generation, record, switch};
pub use config::{existing_user_config_for, user_config_for};

pub type Result<T> = mix_core::Result<T>;
