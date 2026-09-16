use async_trait::async_trait;
use mix_core::paths::{
    DEFAULT_PROFILE_NIX, HOME_MANAGER_PROFILE_NAME, mix_state_dir, nix_profiles_dir,
};
use mix_core::{CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::util::remove_dir_all;
use crate::shared::git;
use crate::shared::home_manager::resolve_user_config;
use crate::shared::os::{path_exists, run_as};

#[derive(Default)]
pub struct ActivateHomeManagerConfig {
    created_git_dir: bool,
}

#[async_trait]
impl Step for ActivateHomeManagerConfig {
    type Error = Error;

    fn name(&self) -> &'static str {
        "activate home-manager config"
    }

    async fn check(&self) -> Result<bool> {
        let Some(cfg) = resolve_user_config() else {
            return Ok(true);
        };
        Ok(path_exists(mix_state_dir(&cfg.user.home).join(".git")).await)
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
        let Some(cfg) = resolve_user_config() else {
            return Ok(());
        };
        let state_dir = mix_state_dir(&cfg.user.home);
        let state_dir_str = state_dir.to_string_lossy().into_owned();

        let flake_attr = format!(
            "path:{state_dir_str}#homeConfigurations.\"{}\".activationPackage",
            cfg.user.name
        );
        let profile = nix_profiles_dir(&cfg.user.home).join(HOME_MANAGER_PROFILE_NAME);
        let profile_str = profile.to_string_lossy().into_owned();
        let store_path = run_as(
            &cfg.user,
            DEFAULT_PROFILE_NIX,
            &[
                "build",
                &flake_attr,
                "--no-link",
                "--print-out-paths",
                "--profile",
                &profile_str,
            ],
            token,
        )
        .await?;

        let activate = format!("{store_path}/activate");
        run_as(&cfg.user, &activate, &[], token).await?;

        self.created_git_dir = !path_exists(state_dir.join(".git")).await;
        if self.created_git_dir {
            git::init(&cfg.user, &state_dir, token).await?;
        }
        git::sync(&cfg.user, &state_dir, token).await?;

        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        let Some(cfg) = resolve_user_config() else {
            return Ok(());
        };
        if self.created_git_dir {
            remove_dir_all(mix_state_dir(&cfg.user.home).join(".git")).await?;
        }
        Ok(())
    }
}
