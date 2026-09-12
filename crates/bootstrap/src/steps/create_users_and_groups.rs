use async_trait::async_trait;
use mix_core::Step;

use crate::constants::{NIXBLD_GID, NIXBLD_GROUP, NIXBLD_UID_BASE, NIXBLD_USER_COUNT};
use crate::error::{Error, Result};
use crate::util::run;

pub struct CreateUsersAndGroups;

#[async_trait]
impl Step for CreateUsersAndGroups {
    type Error = Error;

    fn name(&self) -> &'static str {
        "create nixbld group and build users"
    }

    async fn check(&self) -> Result<bool> {
        Ok(group_has_gid(NIXBLD_GROUP, NIXBLD_GID) && all_users_valid() && all_uids_valid())
    }

    async fn execute(&mut self) -> Result<()> {
        if group_exists(NIXBLD_GROUP) && !group_has_gid(NIXBLD_GROUP, NIXBLD_GID) {
            let gid = NIXBLD_GID.to_string();
            run("groupmod", &["--gid", &gid, NIXBLD_GROUP]).await?;
        } else if !group_exists(NIXBLD_GROUP) {
            let gid = NIXBLD_GID.to_string();
            run("groupadd", &["--system", "--gid", &gid, NIXBLD_GROUP]).await?;
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
                    "/var/empty",
                    "--shell",
                    "/usr/sbin/nologin",
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
        }

        Ok(())
    }
}

pub fn user_name(n: u32) -> String {
    format!("{NIXBLD_GROUP}{n}")
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

pub fn all_users_valid() -> bool {
    (1..=NIXBLD_USER_COUNT).all(|n| user_has_gid(&user_name(n), NIXBLD_GID))
}

pub fn all_uids_valid() -> bool {
    (1..=NIXBLD_USER_COUNT).all(|n| user_has_uid(&user_name(n), NIXBLD_UID_BASE + n))
}
