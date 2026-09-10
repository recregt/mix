use async_trait::async_trait;
use mix_core::{Result, Step};

use crate::constants::{NIXBLD_GID, NIXBLD_GROUP, NIXBLD_UID_BASE, NIXBLD_USER_COUNT};
use crate::util::run;

pub struct CreateUsersAndGroups;

#[async_trait]
impl Step for CreateUsersAndGroups {
    fn name(&self) -> &'static str {
        "create nixbld group and build users"
    }

    async fn check(&self) -> Result<bool> {
        Ok(group_has_gid(NIXBLD_GROUP, NIXBLD_GID) && all_users_exist())
    }

    async fn execute(&mut self) -> Result<()> {
        if !group_exists(NIXBLD_GROUP) {
            let gid = NIXBLD_GID.to_string();
            run("groupadd", &["--system", "--gid", &gid, NIXBLD_GROUP]).await?;
        }

        for n in 1..=NIXBLD_USER_COUNT {
            let name = user_name(n);
            if user_exists(&name) {
                continue;
            }

            let uid = (NIXBLD_UID_BASE + n).to_string();
            let comment = format!("Nix build user {n}");
            run(
                "useradd",
                &[
                    "--system",
                    "--no-create-home",
                    "--home-dir",
                    "/var/empty",
                    "--shell",
                    "/usr/sbin/nologin",
                    "--uid",
                    &uid,
                    "--gid",
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

pub fn all_users_exist() -> bool {
    (1..=NIXBLD_USER_COUNT).all(|n| user_exists(&user_name(n)))
}
