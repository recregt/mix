use mix_core::{Outcome, Plan};

use crate::error::{Error, Result};
use crate::{Environment, planner, preflight, teardown};

async fn interrupted() {
    let _ = tokio::signal::ctrl_c().await;
}

pub async fn bootstrap(mirror: Option<&str>) -> Result<Environment> {
    if !preflight::is_root() {
        return Err(Error::NotRoot("bootstrap the managed environment"));
    }

    preflight::check_not_nixos().await?;
    preflight::check_not_wsl1().await?;
    preflight::check_systemd_ready().await?;
    preflight::check_nix_not_installed().await?;

    run_steps(mirror).await
}

pub async fn reset(mirror: Option<&str>) -> Result<Environment> {
    if !preflight::is_root() {
        return Err(Error::NotRoot("reset the managed environment"));
    }

    preflight::check_not_nixos().await?;
    preflight::check_not_wsl1().await?;
    preflight::check_systemd_ready().await?;
    preflight::check_nix_not_installed().await?;

    teardown::teardown().await?;
    run_steps(mirror).await
}

pub(crate) async fn run_steps(mirror: Option<&str>) -> Result<Environment> {
    let mut plan = Plan::new(planner::bootstrap_steps(mirror));
    let cause = match plan.run_cancellable(interrupted()).await {
        Outcome::Completed(Ok(())) => return Environment::open().await,
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
