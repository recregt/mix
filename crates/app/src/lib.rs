pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod repair;
mod shared;

pub use shared::home_manager::resolve_existing_user_config;
