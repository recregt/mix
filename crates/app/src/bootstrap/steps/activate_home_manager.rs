use std::sync::Arc;

use async_trait::async_trait;
use mix_core::models::UserConfig;
use mix_core::paths::mix_state_dir;
use mix_core::{ActivityReporter, CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::fs;
use crate::profile::{self, BuildPolicy};

pub struct ActivateHomeManagerConfig {
    user_config: Option<UserConfig>,
    mirror: Option<String>,
    mirror_key: Option<String>,
    activity: Arc<dyn ActivityReporter>,
    created_git_dir: bool,
}

impl ActivateHomeManagerConfig {
    pub fn new(
        user_config: Option<UserConfig>,
        mirror: Option<&str>,
        mirror_key: Option<&str>,
        activity: Arc<dyn ActivityReporter>,
    ) -> Self {
        Self {
            user_config,
            mirror: mirror.map(String::from),
            mirror_key: mirror_key.map(String::from),
            activity,
            created_git_dir: false,
        }
    }
}

#[async_trait]
impl Step for ActivateHomeManagerConfig {
    type Error = Error;

    fn name(&self) -> &'static str {
        "activate home-manager config"
    }

    async fn check(&self) -> Result<bool> {
        let Some(cfg) = &self.user_config else {
            return Ok(true);
        };
        Ok(fs::exists(mix_state_dir(&cfg.user.home).join(".git")).await)
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
        let Some(cfg) = &self.user_config else {
            return Ok(());
        };
        // Bootstrapping has to build home-manager's generation from whatever the cache offers,
        // so it is not the place to refuse a build.
        self.created_git_dir = profile::activate(
            cfg,
            self.mirror.as_deref(),
            self.mirror_key.as_deref(),
            &self.activity,
            token,
            BuildPolicy::AllowSource,
        )
        .await?;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        let Some(cfg) = &self.user_config else {
            return Ok(());
        };
        if self.created_git_dir {
            fs::remove_dir_all(mix_state_dir(&cfg.user.home).join(".git")).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mix_core::NoopActivity;
    use mix_core::privilege::InvokingUser;

    use super::*;

    fn noop() -> Arc<dyn ActivityReporter> {
        Arc::new(NoopActivity)
    }

    fn user_config(home: &Path) -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: 4_294_967_294,
                gid: 4_294_967_294,
                name: "mix-user".to_string(),
                home: home.to_path_buf(),
            },
            flake: "flake-content".to_string(),
            home: "home-content".to_string(),
        }
    }

    #[tokio::test]
    async fn check_passes_without_a_user_config() {
        assert!(
            ActivateHomeManagerConfig::new(None, None, None, noop())
                .check()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn check_reports_unconfigured_state_when_git_tracking_is_missing() {
        let home = tempfile::tempdir().unwrap();
        let step =
            ActivateHomeManagerConfig::new(Some(user_config(home.path())), None, None, noop());
        assert!(!step.check().await.unwrap());
    }

    #[tokio::test]
    async fn check_passes_once_the_state_dir_is_git_tracked() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path()).join(".git")).unwrap();
        let step =
            ActivateHomeManagerConfig::new(Some(user_config(home.path())), None, None, noop());
        assert!(step.check().await.unwrap());
    }

    #[tokio::test]
    async fn execute_is_a_no_op_without_a_user_config() {
        let mut step = ActivateHomeManagerConfig::new(None, None, None, noop());
        step.execute(&CancellationToken::new()).await.unwrap();
        assert!(!step.created_git_dir);
    }

    #[tokio::test]
    async fn rollback_is_a_no_op_without_a_user_config() {
        ActivateHomeManagerConfig::new(None, None, None, noop())
            .rollback()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rollback_removes_the_git_dir_it_created() {
        let home = tempfile::tempdir().unwrap();
        let git_dir = mix_state_dir(home.path()).join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut step =
            ActivateHomeManagerConfig::new(Some(user_config(home.path())), None, None, noop());
        step.created_git_dir = true;
        step.rollback().await.unwrap();

        assert!(!git_dir.exists());
        assert!(mix_state_dir(home.path()).exists());
    }

    #[tokio::test]
    async fn rollback_keeps_a_git_dir_it_did_not_create() {
        let home = tempfile::tempdir().unwrap();
        let git_dir = mix_state_dir(home.path()).join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut step =
            ActivateHomeManagerConfig::new(Some(user_config(home.path())), None, None, noop());
        step.rollback().await.unwrap();

        assert!(git_dir.exists());
    }
}
