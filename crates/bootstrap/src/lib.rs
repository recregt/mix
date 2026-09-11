mod bootstrap;
mod constants;
mod doctor;
mod manifest;
mod pins;
mod planner;
mod steps;
mod teardown;
mod util;

pub mod detect;
pub mod preflight;
#[doc(hidden)]
pub mod tarball;

pub use bootstrap::bootstrap;
pub use doctor::doctor;

pub struct Environment(());

impl Environment {
    pub async fn open() -> mix_core::Result<Self> {
        manifest::verify().await?;
        Ok(Self(()))
    }
}
