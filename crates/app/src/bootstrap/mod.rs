mod cleanup;
mod error;
mod planner;
mod steps;

pub mod detect;
pub mod preflight;
#[doc(hidden)]
pub mod tarball;

pub use error::{Error, Host, Result};

use mix_core::{Outcome, Plan, Scope, privilege};

use crate::{Context, HostConfig, Reporters};

pub struct Environment(());

impl Environment {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

pub async fn bootstrap(ctx: &Context, force: bool) -> Result<Environment> {
    let scope = &ctx.scope;
    if !privilege::is_root() {
        return Err(Error::NotRoot("bootstrap the managed environment"));
    }

    preflight::check_not_nixos().await?;
    preflight::check_not_wsl1().await?;
    preflight::check_systemd_ready().await?;
    if !force {
        preflight::check_nix_not_installed(scope).await?;
    }

    run_steps(
        ctx.mirror(),
        ctx.mirror_key(),
        force,
        ctx.reporters.clone(),
        ctx.user.clone(),
        ctx.host.clone(),
        scope,
    )
    .await
}

async fn run_steps(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    reporters: Reporters,
    user_config: Option<mix_core::models::UserConfig>,
    host: HostConfig,
    scope: &Scope,
) -> Result<Environment> {
    let mut plan = Plan::new(planner::bootstrap_steps(
        mirror,
        mirror_key,
        force,
        reporters.downloads,
        reporters.activity,
        user_config,
        host,
    ))
    .with_step_observer(reporters.steps);
    let cause = match plan.run(scope).await {
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
