mod bootstrap;
mod constants;
mod error;
mod health;
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

pub use bootstrap::{bootstrap, reset};
pub use error::{Error, Result};
pub use health::{HealthReport, audit};

pub struct Environment(());

impl Environment {
    pub async fn open() -> Result<Self> {
        manifest::verify().await?;
        Ok(Self(()))
    }
}
