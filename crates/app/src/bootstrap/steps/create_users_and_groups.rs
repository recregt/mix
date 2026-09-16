use async_trait::async_trait;
use mix_core::identity::{
    self, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_HOME, NIXBLD_SHELL, NIXBLD_UID_BASE, NIXBLD_USER_COUNT,
    user_name,
};
use mix_core::{CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::util::{create_dir_all, is_dir, set_permissions, warn_on_failure};
use crate::shared::os::run;

#[derive(Default)]
pub struct CreateUsersAndGroups {
    created_group: bool,
    created_users: Vec<String>,
}

#[async_trait]
impl Step for CreateUsersAndGroups {
    type Error = Error;

    fn name(&self) -> &'static str {
        "create nixbld group and build users"
    }

    async fn check(&self) -> Result<bool> {
        Ok(identity::group_has_gid(NIXBLD_GROUP, NIXBLD_GID) && all_users_valid())
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
        if !is_dir(NIXBLD_HOME).await {
            create_dir_all(NIXBLD_HOME).await?;
            set_permissions(NIXBLD_HOME, 0o555).await?;
        }

        if identity::group_exists(NIXBLD_GROUP)
            && !identity::group_has_gid(NIXBLD_GROUP, NIXBLD_GID)
        {
            let gid = NIXBLD_GID.to_string();
            run("groupmod", &["--gid", &gid, NIXBLD_GROUP], token).await?;
        } else if !identity::group_exists(NIXBLD_GROUP) {
            let gid = NIXBLD_GID.to_string();
            let result = run(
                "groupadd",
                &["--system", "--gid", &gid, NIXBLD_GROUP],
                token,
            )
            .await;
            self.created_group = identity::group_exists(NIXBLD_GROUP);
            result?;
        }

        for n in 1..=NIXBLD_USER_COUNT {
            let name = user_name(n);
            let uid = NIXBLD_UID_BASE + n;
            if identity::user_exists(&name) {
                if !identity::user_has_gid(&name, NIXBLD_GID) {
                    let gid = NIXBLD_GID.to_string();
                    run("usermod", &["--gid", &gid, &name], token).await?;
                }
                if !identity::user_has_uid(&name, uid) {
                    let uid = uid.to_string();
                    run("usermod", &["--uid", &uid, &name], token).await?;
                }
                continue;
            }

            let uid = uid.to_string();
            let comment = format!("mix build user {n}");
            let result = run(
                "useradd",
                &[
                    "--system",
                    "--no-create-home",
                    "--no-user-group",
                    "--home-dir",
                    NIXBLD_HOME,
                    "--shell",
                    NIXBLD_SHELL,
                    "--uid",
                    &uid,
                    "--gid",
                    NIXBLD_GROUP,
                    "--groups",
                    NIXBLD_GROUP,
                    "--comment",
                    &comment,
                    &name,
                ],
                token,
            )
            .await;
            if identity::user_exists(&name) {
                self.created_users.push(name);
            }
            result?;
        }

        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        let token = CancellationToken::new();

        for name in self.created_users.drain(..).rev() {
            delete_user(&name).await;
        }

        if self.created_group {
            warn_on_failure(
                "delete nixbld group",
                run("groupdel", &[NIXBLD_GROUP], &token).await,
            );
            self.created_group = false;
        }

        Ok(())
    }
}

pub(crate) async fn delete_user(name: &str) {
    terminate_processes(name).await;
    let token = CancellationToken::new();
    warn_on_failure("delete build user", run("userdel", &[name], &token).await);
}

async fn terminate_processes(name: &str) {
    match tokio::process::Command::new("pkill")
        .args(["-u", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
    {
        Ok(status) if status.success() || status.code() == Some(1) => {}
        Ok(status) => tracing::warn!("pkill -u {name} exited with {status}, continuing"),
        Err(e) => tracing::warn!("pkill -u {name} failed to run: {e}, continuing"),
    }
}

fn all_users_valid() -> bool {
    (1..=NIXBLD_USER_COUNT)
        .all(|n| identity::user_matches(&user_name(n), NIXBLD_UID_BASE + n, NIXBLD_GID))
}
