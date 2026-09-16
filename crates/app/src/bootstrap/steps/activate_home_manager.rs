use async_trait::async_trait;
use mix_core::models::UserConfig;
use mix_core::paths::{
    DEFAULT_PROFILE_NIX, HOME_MANAGER_PROFILE_NAME, mix_state_dir, nix_profiles_dir,
};
use mix_core::{CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::mirror;
use crate::bootstrap::util::remove_dir_all;
use crate::shared::git;
use crate::shared::os::{path_exists, run_as};

pub struct ActivateHomeManagerConfig {
    user_config: Option<UserConfig>,
    mirror: Option<String>,
    created_git_dir: bool,
}

impl ActivateHomeManagerConfig {
    pub fn new(user_config: Option<UserConfig>, mirror: Option<&str>) -> Self {
        Self {
            user_config,
            mirror: mirror.map(String::from),
            created_git_dir: false,
        }
    }
}

async fn nix_build_args(flake_attr: &str, profile_str: &str, mirror: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "build".to_string(),
        flake_attr.to_string(),
        "--no-link".to_string(),
        "--print-out-paths".to_string(),
        "--profile".to_string(),
        profile_str.to_string(),
    ];

    if let Some(base) = mirror::filter_mirror(mirror) {
        args.push("--override-input".to_string());
        args.push("nixpkgs".to_string());
        args.push(mirror::nixpkgs_override(base));
        args.push("--override-input".to_string());
        args.push("home-manager".to_string());
        args.push(mirror::home_manager_override(base));
        args.push("--option".to_string());
        args.push("substituters".to_string());
        args.push(mirror::substituter(base));
        args.push("--option".to_string());
        args.push("trusted-public-keys".to_string());
        args.push(mirror::trusted_public_keys(base).await);
    }

    args
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
        Ok(path_exists(mix_state_dir(&cfg.user.home).join(".git")).await)
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
        let Some(cfg) = &self.user_config else {
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
        let args = nix_build_args(&flake_attr, &profile_str, self.mirror.as_deref()).await;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let store_path = run_as(&cfg.user, DEFAULT_PROFILE_NIX, &arg_refs, token).await?;

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
        let Some(cfg) = &self.user_config else {
            return Ok(());
        };
        if self.created_git_dir {
            remove_dir_all(mix_state_dir(&cfg.user.home).join(".git")).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mix_core::privilege::InvokingUser;

    use super::*;

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
            ActivateHomeManagerConfig::new(None, None)
                .check()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn check_reports_unconfigured_state_when_git_tracking_is_missing() {
        let home = tempfile::tempdir().unwrap();
        let step = ActivateHomeManagerConfig::new(Some(user_config(home.path())), None);
        assert!(!step.check().await.unwrap());
    }

    #[tokio::test]
    async fn check_passes_once_the_state_dir_is_git_tracked() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path()).join(".git")).unwrap();
        let step = ActivateHomeManagerConfig::new(Some(user_config(home.path())), None);
        assert!(step.check().await.unwrap());
    }

    #[tokio::test]
    async fn execute_is_a_no_op_without_a_user_config() {
        let mut step = ActivateHomeManagerConfig::new(None, None);
        step.execute(&CancellationToken::new()).await.unwrap();
        assert!(!step.created_git_dir);
    }

    #[tokio::test]
    async fn rollback_is_a_no_op_without_a_user_config() {
        ActivateHomeManagerConfig::new(None, None)
            .rollback()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rollback_removes_the_git_dir_it_created() {
        let home = tempfile::tempdir().unwrap();
        let git_dir = mix_state_dir(home.path()).join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut step = ActivateHomeManagerConfig::new(Some(user_config(home.path())), None);
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

        let mut step = ActivateHomeManagerConfig::new(Some(user_config(home.path())), None);
        step.rollback().await.unwrap();

        assert!(git_dir.exists());
    }

    const UNREACHABLE_MIRROR: &str = "http://127.0.0.1:1";

    #[tokio::test]
    async fn nix_build_args_omits_mirror_flags_when_no_mirror_is_set() {
        let args = nix_build_args("path:/state#x", "/profile", None).await;
        assert!(!args.iter().any(|a| a == "--override-input"));
        assert!(!args.iter().any(|a| a == "substituters"));
    }

    #[tokio::test]
    async fn nix_build_args_adds_override_inputs_and_a_substituter_when_mirrored() {
        let args = nix_build_args("path:/state#x", "/profile", Some(UNREACHABLE_MIRROR)).await;
        assert!(args.iter().any(|a| a == "nixpkgs"));
        assert!(args.iter().any(|a| a == "home-manager"));
        assert!(
            args.iter()
                .any(|a| a.starts_with(&format!("tarball+{UNREACHABLE_MIRROR}/nixpkgs-")))
        );
        assert!(
            args.iter()
                .any(|a| a.starts_with(&format!("tarball+{UNREACHABLE_MIRROR}/home-manager-")))
        );
        assert!(
            args.iter()
                .any(|a| a == &format!("{UNREACHABLE_MIRROR}/cache"))
        );
        assert!(args.iter().any(|a| a == "trusted-public-keys"));
        assert!(args.iter().any(|a| a.starts_with("cache.nixos.org-1:")));
    }

    #[tokio::test]
    async fn nix_build_args_always_start_with_the_build_invocation() {
        for mirror in [None, Some(UNREACHABLE_MIRROR)] {
            let args = nix_build_args("path:/state#x", "/profile", mirror).await;
            assert_eq!(
                args[..6],
                [
                    "build",
                    "path:/state#x",
                    "--no-link",
                    "--print-out-paths",
                    "--profile",
                    "/profile"
                ]
            );
        }
    }

    #[tokio::test]
    async fn nix_build_args_ignores_a_blank_mirror() {
        let args = nix_build_args("path:/state#x", "/profile", Some("   ")).await;
        assert!(!args.iter().any(|a| a == "--override-input"));
    }
}
