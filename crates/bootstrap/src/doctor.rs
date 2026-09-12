use crate::error::{Error, Result};
use crate::{Environment, bootstrap, preflight, teardown};

pub async fn doctor(mirror: Option<&str>) -> Result<Environment> {
    if !preflight::is_root() {
        return Err(Error::NotRoot("reset the managed environment"));
    }

    preflight::check_not_nixos()?;
    preflight::check_not_wsl1()?;
    preflight::check_systemd_ready()?;
    preflight::check_nix_not_installed().await?;

    teardown::teardown().await?;
    bootstrap::run_steps(mirror).await
}
