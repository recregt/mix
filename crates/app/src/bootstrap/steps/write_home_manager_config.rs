use async_trait::async_trait;
use mix_core::models::user_targets;
use mix_core::paths::{mix_state_dir, mix_user_marker};
use mix_core::{CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::util::{is_file, remove_dir_all, remove_file};
use crate::home_manager::resolve_user_config;
use crate::os::path_exists;

#[derive(Default)]
pub struct WriteHomeManagerConfig {
    created_dir: bool,
}

#[async_trait]
impl Step for WriteHomeManagerConfig {
    type Error = Error;

    fn name(&self) -> &'static str {
        "write home-manager config"
    }

    async fn check(&self) -> Result<bool> {
        let Some(cfg) = resolve_user_config() else {
            return Ok(true);
        };
        Ok(is_file(mix_user_marker(cfg.user.uid)).await)
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
        let Some(cfg) = resolve_user_config() else {
            return Ok(());
        };
        let state_dir = mix_state_dir(&cfg.user.home);

        self.created_dir = !path_exists(&state_dir).await;
        for target in user_targets(&cfg) {
            crate::repair::fix(&target, token).await?;
        }
        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        let Some(cfg) = resolve_user_config() else {
            return Ok(());
        };
        let state_dir = mix_state_dir(&cfg.user.home);

        if self.created_dir {
            remove_dir_all(&state_dir).await?;
        } else {
            remove_file(state_dir.join("flake.nix")).await?;
            remove_file(state_dir.join("home.nix")).await?;
        }
        remove_file(mix_user_marker(cfg.user.uid)).await?;
        Ok(())
    }
}
