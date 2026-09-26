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

use mix_core::privilege::InvokingUser;
use mix_core::{ActivityReporter, DownloadProgress, Outcome, Plan, StepObserver, privilege};

pub struct Environment(());

impl Environment {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

pub struct Reporters {
    pub downloads: Arc<dyn DownloadProgress>,
    pub steps: Arc<dyn StepObserver>,
    pub activity: Arc<dyn ActivityReporter>,
}

async fn interrupted(stop: impl Future<Output = ()>) {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("registering a SIGTERM handler");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
        _ = stop => {}
    }
    tracing::warn!("Cancelling... (cleaning up)");
}

pub async fn bootstrap(
    user: Option<InvokingUser>,
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    reporters: Reporters,
    stop: impl Future<Output = ()>,
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

    let user_config = user.and_then(crate::profile::user_config_for);
    run_steps(mirror, mirror_key, force, reporters, user_config, stop).await
}

async fn run_steps(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    reporters: Reporters,
    user_config: Option<mix_core::models::UserConfig>,
    stop: impl Future<Output = ()>,
) -> Result<Environment> {
    let mut plan = Plan::new(planner::bootstrap_steps(
        mirror,
        mirror_key,
        force,
        reporters.downloads,
        reporters.activity,
        user_config,
    ))
    .with_step_observer(reporters.steps);
    let cause = match plan.run_cancellable(interrupted(stop)).await {
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
        let handle = tokio::spawn(interrupted(std::future::pending()));
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

    #[tokio::test]
    async fn interrupted_resolves_once_the_caller_asks_to_stop() {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(interrupted(async {
            let _ = stopped.await;
        }));
        drop(stop);

        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("interrupted() should resolve once the stop future does")
            .unwrap();
    }
}
