use mix_core::{Error, Plan, Result};

use crate::{Environment, planner, preflight};

pub async fn bootstrap() -> Result<Environment> {
    if !preflight::is_root() {
        return Err(Error::NotRoot("bootstrap the managed environment"));
    }

    preflight::check_not_nixos()?;
    preflight::check_not_wsl1()?;
    preflight::check_systemd_ready()?;
    preflight::check_nix_not_installed().await?;

    Plan::new(planner::bootstrap_steps()).run().await?;

    Environment::open().await
}
