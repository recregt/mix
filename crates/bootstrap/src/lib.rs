mod bootstrap;
mod constants;
pub mod detect;
mod doctor;
mod manifest;
mod pins;
mod planner;
pub mod preflight;
mod steps;
/// Archive handling internals, exposed for benchmarks only. Not a stable API.
#[doc(hidden)]
pub mod tarball;
mod teardown;
mod util;

pub use bootstrap::bootstrap;
pub use doctor::doctor;

pub struct Environment(());

impl Environment {
    pub async fn open() -> mix_core::Result<Self> {
        manifest::verify().await?;
        Ok(Self(()))
    }
}
