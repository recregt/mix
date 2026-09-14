mod error;
mod pins;
mod planner;
mod steps;
mod util;

pub mod detect;
pub mod preflight;
#[doc(hidden)]
pub mod tarball;

pub use error::{Error, Result};

use mix_core::{Outcome, Plan, privilege};

pub struct Environment(());

impl Environment {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

async fn interrupted() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("registering a SIGTERM handler");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::warn!("Cancelling... (cleaning up)");
}

pub async fn bootstrap(mirror: Option<&str>) -> Result<Environment> {
    if !privilege::is_root() {
        return Err(Error::NotRoot("bootstrap the managed environment"));
    }

    preflight::check_not_nixos().await?;
    preflight::check_not_wsl1().await?;
    preflight::check_systemd_ready().await?;
    preflight::check_nix_not_installed().await?;

    run_steps(mirror).await
}

async fn run_steps(mirror: Option<&str>) -> Result<Environment> {
    let mut plan = Plan::new(planner::bootstrap_steps(mirror));
    let cause = match plan.run_cancellable(interrupted()).await {
        Outcome::Completed(Ok(())) => return Ok(Environment::new()),
        Outcome::Completed(Err(cause)) => cause,
        Outcome::Interrupted => Error::Interrupted,
    };

    let failed_rollbacks = plan.failed_rollbacks();
    if failed_rollbacks.is_empty() {
        return Err(cause);
    }
    Err(Error::Rollback {
        cause: Box::new(cause),
        summary: format!(
            "{} rollback step(s) failed, the system may need manual cleanup: {}",
            failed_rollbacks.len(),
            failed_rollbacks.join("; ")
        ),
    })
}
