mod cleanup;
mod error;
mod planner;
mod steps;

pub mod detect;
pub mod preflight;
#[doc(hidden)]
pub mod tarball;

pub use error::{Error, Host, Result};

use std::sync::Arc;

use mix_core::{ActivityReporter, DownloadProgress, Outcome, Plan, StepObserver, privilege};

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

pub async fn bootstrap(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    progress: Arc<dyn DownloadProgress>,
    step_observer: Arc<dyn StepObserver>,
    activity: Arc<dyn ActivityReporter>,
) -> Result<Environment> {
    if !privilege::is_root() {
        return Err(Error::NotRoot("bootstrap the managed environment"));
    }

    preflight::check_not_nixos().await?;
    preflight::check_not_wsl1().await?;
    preflight::check_systemd_ready().await?;
    if !force {
        preflight::check_nix_not_installed().await?;
    }

    run_steps(mirror, mirror_key, force, progress, step_observer, activity).await
}

async fn run_steps(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    progress: Arc<dyn DownloadProgress>,
    step_observer: Arc<dyn StepObserver>,
    activity: Arc<dyn ActivityReporter>,
) -> Result<Environment> {
    let mut plan = Plan::new(planner::bootstrap_steps(
        mirror, mirror_key, force, progress, activity,
    ))
    .with_step_observer(step_observer);
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
            "{} rollback step(s) failed: {}",
            failed_rollbacks.len(),
            failed_rollbacks.join("; ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nix::sys::signal::{self, Signal};

    use super::*;

    async fn assert_interrupted_by(raise: Signal) {
        let handle = tokio::spawn(interrupted());
        tokio::time::sleep(Duration::from_millis(50)).await;
        signal::raise(raise).unwrap();

        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("interrupted() should resolve once the signal is raised")
            .unwrap();
    }

    #[tokio::test]
    async fn interrupted_resolves_on_sigint() {
        assert_interrupted_by(Signal::SIGINT).await;
    }

    #[tokio::test]
    async fn interrupted_resolves_on_sigterm() {
        assert_interrupted_by(Signal::SIGTERM).await;
    }
}
