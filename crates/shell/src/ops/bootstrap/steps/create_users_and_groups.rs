use async_trait::async_trait;
use mix_core::identity::{
    MIX_USERS_GID, MIX_USERS_GROUP, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_HOME, NIXBLD_SHELL,
    NIXBLD_UID_BASE, NIXBLD_USER_COUNT, user_name,
};
use mix_core::{Scope, Step};

use crate::effect::exec::run;
use crate::effect::fs::{create_dir_all, is_dir, set_mode};
use crate::ops::bootstrap::cleanup::warn_on_failure;
use crate::ops::bootstrap::error::{Error, Result};

#[derive(Default)]
pub struct CreateUsersAndGroups {
    created_groups: Vec<&'static str>,
    created_users: Vec<String>,
}

#[async_trait]
impl Step for CreateUsersAndGroups {
    type Error = Error;

    fn name(&self) -> &'static str {
        "create the managed groups and build users"
    }

    async fn check(&self, _scope: &Scope) -> Result<bool> {
        Ok(
            crate::effect::accounts::group_has_gid(NIXBLD_GROUP, NIXBLD_GID)
                && crate::effect::accounts::group_has_gid(MIX_USERS_GROUP, MIX_USERS_GID)
                && all_users_valid(),
        )
    }

    async fn execute(&mut self, scope: &Scope) -> Result<()> {
        if !is_dir(NIXBLD_HOME).await {
            create_dir_all(NIXBLD_HOME).await?;
            set_mode(NIXBLD_HOME, 0o555).await?;
        }

        self.reconcile_group(NIXBLD_GROUP, NIXBLD_GID, scope)
            .await?;
        self.reconcile_group(MIX_USERS_GROUP, MIX_USERS_GID, scope)
            .await?;

        for n in 1..=NIXBLD_USER_COUNT {
            let name = user_name(n);
            let uid = NIXBLD_UID_BASE + n;
            if crate::effect::accounts::user_exists(&name) {
                if !crate::effect::accounts::user_has_gid(&name, NIXBLD_GID) {
                    let gid = NIXBLD_GID.to_string();
                    run("usermod", &["--gid", &gid, &name], scope).await?;
                }
                if !crate::effect::accounts::user_has_uid(&name, uid) {
                    let uid = uid.to_string();
                    run("usermod", &["--uid", &uid, &name], scope).await?;
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
                scope,
            )
            .await;
            if crate::effect::accounts::user_exists(&name) {
                self.created_users.push(name.into_owned());
            }
            result?;
        }

        Ok(())
    }

    async fn rollback(&mut self, scope: &Scope) -> Result<()> {
        for name in self.created_users.drain(..).rev() {
            delete_user(&name, scope).await;
        }

        for name in self.created_groups.drain(..).rev() {
            warn_on_failure("delete group", run("groupdel", &[name], scope).await);
        }

        Ok(())
    }
}

impl CreateUsersAndGroups {
    async fn reconcile_group(&mut self, name: &'static str, gid: u32, scope: &Scope) -> Result<()> {
        if crate::effect::accounts::group_exists(name) {
            if !crate::effect::accounts::group_has_gid(name, gid) {
                let gid = gid.to_string();
                run("groupmod", &["--gid", &gid, name], scope).await?;
            }
            return Ok(());
        }

        let gid = gid.to_string();
        let result = run("groupadd", &["--system", "--gid", &gid, name], scope).await;
        if crate::effect::accounts::group_exists(name) {
            self.created_groups.push(name);
        }
        result?;
        Ok(())
    }
}

pub(crate) async fn delete_user(name: &str, scope: &Scope) {
    terminate_processes(name, scope).await;
    warn_on_failure("delete build user", run("userdel", &[name], scope).await);
}

async fn terminate_processes(name: &str, scope: &Scope) {
    match mix_exec::Command::new("pkill")
        .args(["-u", name])
        .output(scope)
        .await
    {
        Ok(output) if output.status.success() || output.status.code() == Some(1) => {}
        Ok(output) => tracing::warn!("pkill -u {name} exited with {}, continuing", output.status),
        Err(e) => tracing::warn!("pkill -u {name} failed to run: {e}, continuing"),
    }
}

fn all_users_valid() -> bool {
    (1..=NIXBLD_USER_COUNT).all(|n| {
        crate::effect::accounts::user_matches(&user_name(n), NIXBLD_UID_BASE + n, NIXBLD_GID)
    })
}
