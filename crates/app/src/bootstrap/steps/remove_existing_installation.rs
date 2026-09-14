use async_trait::async_trait;
use mix_core::identity::{self, NIXBLD_GROUP, NIXBLD_USER_COUNT, user_name};
use mix_core::paths::{
    NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SOCKET_DEST, PROFILE_SNIPPET_DEST,
};
use mix_core::{CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::steps::create_users_and_groups::delete_user;
use crate::bootstrap::util::{remove_dir_all, remove_file, warn_on_failure};
use crate::os::run;

#[derive(Default)]
pub struct RemoveExistingInstallation;

#[async_trait]
impl Step for RemoveExistingInstallation {
    type Error = Error;

    fn name(&self) -> &'static str {
        "remove the existing installation"
    }

    async fn check(&self) -> Result<bool> {
        Ok(false)
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
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
                token,
            )
            .await,
        );
        remove_file(NIX_DAEMON_SERVICE_DEST).await?;
        remove_file(NIX_DAEMON_SOCKET_DEST).await?;
        warn_on_failure(
            "reload systemd",
            run("systemctl", &["daemon-reload"], token).await,
        );

        for n in 1..=NIXBLD_USER_COUNT {
            let name = user_name(n);
            if identity::user_exists(&name) {
                delete_user(&name).await;
            }
        }
        if identity::group_exists(NIXBLD_GROUP) {
            warn_on_failure(
                "delete nixbld group",
                run("groupdel", &[NIXBLD_GROUP], token).await,
            );
        }

        remove_file(NIX_CONF_DEST).await?;
        remove_file(PROFILE_SNIPPET_DEST).await?;
        remove_dir_all("/nix").await?;

        Ok(())
    }
}
