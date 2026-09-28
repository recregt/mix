use async_trait::async_trait;
use mix_core::identity::{MIX_USERS_GROUP, NIXBLD_GROUP, NIXBLD_USER_COUNT, user_name};
use mix_core::paths::{
    NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SOCKET_DEST, PROFILE_SNIPPET_DEST,
};
use mix_core::{Scope, Step};

use crate::bootstrap::cleanup::warn_on_failure;
use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::steps::create_users_and_groups::delete_user;
use crate::exec::run;
use crate::fs::{remove_dir_all, remove_file};

#[derive(Default)]
pub struct RemoveExistingInstallation;

#[async_trait]
impl Step for RemoveExistingInstallation {
    type Error = Error;

    fn name(&self) -> &'static str {
        "remove the existing installation"
    }

    async fn check(&self, _scope: &Scope) -> Result<bool> {
        Ok(false)
    }

    async fn execute(&mut self, scope: &Scope) -> Result<()> {
        warn_on_failure(
            "disable nix-daemon.socket",
            run(
                "systemctl",
                &[
                    "disable",
                    "--now",
                    "nix-daemon.socket",
                    "nix-daemon.service",
                ],
                scope,
            )
            .await,
        );
        remove_file(NIX_DAEMON_SERVICE_DEST).await?;
        remove_file(NIX_DAEMON_SOCKET_DEST).await?;
        warn_on_failure(
            "reload systemd",
            run("systemctl", &["daemon-reload"], scope).await,
        );

        for n in 1..=NIXBLD_USER_COUNT {
            let name = user_name(n);
            if crate::accounts::user_exists(&name) {
                delete_user(&name, scope).await;
            }
        }
        for group in [NIXBLD_GROUP, MIX_USERS_GROUP] {
            if crate::accounts::group_exists(group) {
                warn_on_failure("delete group", run("groupdel", &[group], scope).await);
            }
        }

        remove_file(NIX_CONF_DEST).await?;
        remove_file(PROFILE_SNIPPET_DEST).await?;
        remove_dir_all("/nix").await?;

        Ok(())
    }
}
