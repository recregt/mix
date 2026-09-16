use async_trait::async_trait;
use mix_core::identity::{self, MIX_USERS_GROUP};
use mix_core::models::{UserConfig, user_targets};
use mix_core::paths::{FLAKE_NIX, HOME_NIX, mix_state_dir};
use mix_core::{CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::util::{remove_dir_all, remove_file, warn_on_failure};
use crate::shared::os::{path_exists, run};

pub struct WriteHomeManagerConfig {
    user_config: Option<UserConfig>,
    created_dir: bool,
    enrolled: bool,
}

impl WriteHomeManagerConfig {
    pub fn new(user_config: Option<UserConfig>) -> Self {
        Self {
            user_config,
            created_dir: false,
            enrolled: false,
        }
    }
}

#[async_trait]
impl Step for WriteHomeManagerConfig {
    type Error = Error;

    fn name(&self) -> &'static str {
        "write home-manager config"
    }

    async fn check(&self) -> Result<bool> {
        let Some(cfg) = &self.user_config else {
            return Ok(true);
        };
        Ok(identity::group_has_member(MIX_USERS_GROUP, &cfg.user.name))
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
        let Some(cfg) = &self.user_config else {
            return Ok(());
        };
        let state_dir = mix_state_dir(&cfg.user.home);

        self.created_dir = !path_exists(&state_dir).await;
        self.enrolled = !identity::group_has_member(MIX_USERS_GROUP, &cfg.user.name);
        for target in user_targets(cfg) {
            crate::repair::fix(&target, token).await?;
        }
        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        let Some(cfg) = &self.user_config else {
            return Ok(());
        };
        let state_dir = mix_state_dir(&cfg.user.home);

        if self.created_dir {
            remove_dir_all(&state_dir).await?;
        } else {
            remove_file(state_dir.join(FLAKE_NIX)).await?;
            remove_file(state_dir.join(HOME_NIX)).await?;
        }

        if self.enrolled {
            let token = CancellationToken::new();
            warn_on_failure(
                "un-enrol the user from the managed group",
                run(
                    "gpasswd",
                    &["--delete", &cfg.user.name, MIX_USERS_GROUP],
                    &token,
                )
                .await,
            );
            self.enrolled = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mix_core::paths::MIX_STATE_DIR;
    use mix_core::privilege::InvokingUser;

    use super::*;

    const UNUSED_UID: u32 = 4_294_967_294;

    fn user_config(home: &Path) -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: UNUSED_UID,
                gid: UNUSED_UID,
                name: "mix-user".to_string(),
                home: home.to_path_buf(),
            },
            flake: "flake-content".to_string(),
            home: "home-content".to_string(),
        }
    }

    fn populated_state_dir(home: &Path) -> std::path::PathBuf {
        let state_dir = mix_state_dir(home);
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();
        std::fs::write(state_dir.join(HOME_NIX), "home-content").unwrap();
        state_dir
    }

    #[tokio::test]
    async fn check_passes_without_a_user_config() {
        assert!(WriteHomeManagerConfig::new(None).check().await.unwrap());
    }

    #[tokio::test]
    async fn check_reports_an_unenrolled_user_as_unconfigured() {
        let home = tempfile::tempdir().unwrap();
        let step = WriteHomeManagerConfig::new(Some(user_config(home.path())));
        assert!(!step.check().await.unwrap());
    }

    #[tokio::test]
    async fn execute_is_a_no_op_without_a_user_config() {
        let mut step = WriteHomeManagerConfig::new(None);
        step.execute(&CancellationToken::new()).await.unwrap();
        assert!(!step.created_dir);
    }

    #[tokio::test]
    async fn rollback_is_a_no_op_without_a_user_config() {
        WriteHomeManagerConfig::new(None).rollback().await.unwrap();
    }

    #[tokio::test]
    async fn rollback_removes_the_whole_state_dir_when_it_created_it() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = populated_state_dir(home.path());

        let mut step = WriteHomeManagerConfig::new(Some(user_config(home.path())));
        step.created_dir = true;
        step.rollback().await.unwrap();

        assert!(!state_dir.exists());
        assert!(home.path().join(".local/state").exists());
    }

    #[tokio::test]
    async fn rollback_removes_only_the_generated_files_when_the_state_dir_pre_existed() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = populated_state_dir(home.path());
        std::fs::write(state_dir.join("flake.lock"), "lock").unwrap();

        let mut step = WriteHomeManagerConfig::new(Some(user_config(home.path())));
        step.rollback().await.unwrap();

        assert!(state_dir.exists());
        assert!(!state_dir.join(FLAKE_NIX).exists());
        assert!(!state_dir.join(HOME_NIX).exists());
        assert!(state_dir.join("flake.lock").exists());
    }

    #[tokio::test]
    async fn rollback_tolerates_a_state_dir_that_was_never_written() {
        let home = tempfile::tempdir().unwrap();
        let mut step = WriteHomeManagerConfig::new(Some(user_config(home.path())));
        step.rollback().await.unwrap();
        assert!(!home.path().join(MIX_STATE_DIR).exists());
    }
}
