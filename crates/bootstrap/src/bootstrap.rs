use mix_core::{Plan, Result};

use crate::preflight::RootStatus;
use crate::{Environment, Outcome, planner, preflight};

pub async fn bootstrap() -> Result<Outcome> {
    if let RootStatus::ReExecuted { exit_code } = preflight::ensure_root("bootstrap Nix")? {
        return Ok(Outcome::ReExecuted { exit_code });
    }

    preflight::check_not_nixos()?;
    preflight::check_not_wsl1()?;
    preflight::check_systemd_ready()?;
    preflight::check_nix_not_installed().await?;

    Plan::new(planner::bootstrap_steps()).run().await?;

    Ok(Outcome::Bootstrapped(Environment::open().await?))
}
