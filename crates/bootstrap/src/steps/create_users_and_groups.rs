use async_trait::async_trait;
use mix_core::Step;

use crate::constants::{
    NIXBLD_GID, NIXBLD_GROUP, NIXBLD_HOME, NIXBLD_SHELL, NIXBLD_UID_BASE, NIXBLD_USER_COUNT,
};
use crate::error::{Error, Result};
use crate::util::{create_dir_all, is_dir, run, set_permissions, warn_on_failure};

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
        Ok(group_has_gid(NIXBLD_GROUP, NIXBLD_GID) && all_users_valid())
    }

    async fn execute(&mut self) -> Result<()> {
        if !is_dir(NIXBLD_HOME).await {
            create_dir_all(NIXBLD_HOME).await?;
            set_permissions(NIXBLD_HOME, 0o555).await?;
        }

        if group_exists(NIXBLD_GROUP) && !group_has_gid(NIXBLD_GROUP, NIXBLD_GID) {
            let gid = NIXBLD_GID.to_string();
            run("groupmod", &["--gid", &gid, NIXBLD_GROUP]).await?;
        } else if !group_exists(NIXBLD_GROUP) {
            let gid = NIXBLD_GID.to_string();
            run("groupadd", &["--system", "--gid", &gid, NIXBLD_GROUP]).await?;
            self.created_group = true;
        }

        for n in 1..=NIXBLD_USER_COUNT {
            let name = user_name(n);
            let uid = NIXBLD_UID_BASE + n;
            if user_exists(&name) {
                if !user_has_gid(&name, NIXBLD_GID) {
                    let gid = NIXBLD_GID.to_string();
                    run("usermod", &["--gid", &gid, &name]).await?;
                }
                if !user_has_uid(&name, uid) {
                    let uid = uid.to_string();
                    run("usermod", &["--uid", &uid, &name]).await?;
                }
                continue;
            }

            let uid = uid.to_string();
            let comment = format!("mix build user {n}");
            run(
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
            )
            .await?;
            self.created_users.push(name);
        }

        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        for name in self.created_users.drain(..).rev() {
            delete_user(&name).await;
        }

        if self.created_group {
            warn_on_failure(
                "delete nixbld group",
                run("groupdel", &[NIXBLD_GROUP]).await,
            );
            self.created_group = false;
        }

        Ok(())
    }
}

pub fn user_name(n: u32) -> String {
    format!("{NIXBLD_GROUP}{n}")
}

pub(crate) async fn delete_user(name: &str) {
    terminate_processes(name).await;
    warn_on_failure("delete build user", run("userdel", &[name]).await);
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

pub fn group_exists(name: &str) -> bool {
    nix::unistd::Group::from_name(name).ok().flatten().is_some()
}

pub fn group_has_gid(name: &str, gid: u32) -> bool {
    nix::unistd::Group::from_name(name)
        .ok()
        .flatten()
        .is_some_and(|group| group.gid.as_raw() == gid)
}

pub fn user_exists(name: &str) -> bool {
    nix::unistd::User::from_name(name).ok().flatten().is_some()
}

pub fn user_has_gid(name: &str, gid: u32) -> bool {
    nix::unistd::User::from_name(name)
        .ok()
        .flatten()
        .is_some_and(|user| user.gid.as_raw() == gid)
}

pub fn user_has_uid(name: &str, uid: u32) -> bool {
    nix::unistd::User::from_name(name)
        .ok()
        .flatten()
        .is_some_and(|user| user.uid.as_raw() == uid)
}

pub fn user_matches(name: &str, uid: u32, gid: u32) -> bool {
    nix::unistd::User::from_name(name)
        .ok()
        .flatten()
        .is_some_and(|user| user.uid.as_raw() == uid && user.gid.as_raw() == gid)
}

pub fn all_users_valid() -> bool {
    (1..=NIXBLD_USER_COUNT).all(|n| user_matches(&user_name(n), NIXBLD_UID_BASE + n, NIXBLD_GID))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_matches_true_for_a_known_system_user() {
        assert!(user_matches("root", 0, 0));
    }

    #[test]
    fn user_matches_false_for_the_wrong_uid() {
        assert!(!user_matches("root", 1, 0));
    }

    #[test]
    fn user_matches_false_for_the_wrong_gid() {
        assert!(!user_matches("root", 0, 1));
    }

    #[test]
    fn user_matches_false_for_a_nonexistent_user() {
        assert!(!user_matches("mix-test-nonexistent-user-xyz", 0, 0));
    }
}
