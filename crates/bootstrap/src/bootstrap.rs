use mix_core::Plan;

use crate::error::{Error, Result};
use crate::{Environment, planner, preflight};

pub async fn bootstrap(mirror: Option<&str>) -> Result<Environment> {
    if !preflight::is_root() {
        return Err(Error::NotRoot("bootstrap the managed environment"));
    }

    preflight::check_not_nixos()?;
    preflight::check_not_wsl1()?;
    preflight::check_systemd_ready()?;
    preflight::check_nix_not_installed().await?;

    run_steps(mirror).await
}

pub(crate) async fn run_steps(mirror: Option<&str>) -> Result<Environment> {
    Plan::new(planner::bootstrap_steps(mirror))
        .run()
        .await
        .map_err(enrich_network_error)?;

    Ok(Environment::open().await?)
}

fn enrich_network_error(e: mix_core::Error) -> Error {
    match e {
        mix_core::Error::Network(source) => Error::Network(source),
        other => Error::Core(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrich_network_error_adds_the_mirror_hint() {
        let boxed: Box<dyn std::error::Error + Send + Sync> = "timed out".into();
        let err = enrich_network_error(mix_core::Error::Network(boxed));

        assert!(matches!(err, Error::Network(_)));
        assert!(err.to_string().contains("--mirror"));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn enrich_network_error_passes_other_variants_through_as_core() {
        let err = enrich_network_error(mix_core::Error::Decompression("bad xz".into()));

        assert!(matches!(
            err,
            Error::Core(mix_core::Error::Decompression(_))
        ));
    }
}
