use mix_core::{Plan, Result};

use crate::{planner, preflight};

pub async fn install() -> Result<()> {
    preflight::ensure_root("install Nix")?;
    preflight::check_not_nixos()?;
    preflight::check_not_wsl1()?;
    preflight::check_systemd_ready()?;
    preflight::check_nix_not_installed().await?;

    Plan::new(planner::install_steps()).run().await
}
