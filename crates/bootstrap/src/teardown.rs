use mix_core::Result;

use crate::constants::{
    NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SOCKET_DEST, NIXBLD_GROUP,
    NIXBLD_USER_COUNT, PROFILE_SNIPPET_DEST,
};
use crate::steps::create_users_and_groups::user_name;
use crate::util::run;

pub async fn teardown() -> Result<()> {
    let _ = run("systemctl", &["disable", "--now", "nix-daemon.socket"]).await;
    let _ = run("systemctl", &["disable", "--now", "nix-daemon.service"]).await;
    let _ = tokio::fs::remove_file(NIX_DAEMON_SOCKET_DEST).await;
    let _ = tokio::fs::remove_file(NIX_DAEMON_SERVICE_DEST).await;
    let _ = run("systemctl", &["daemon-reload"]).await;

    for n in 1..=NIXBLD_USER_COUNT {
        let _ = run("userdel", &[&user_name(n)]).await;
    }
    let _ = run("groupdel", &[NIXBLD_GROUP]).await;

    let _ = tokio::fs::remove_file(NIX_CONF_DEST).await;
    let _ = tokio::fs::remove_file(PROFILE_SNIPPET_DEST).await;

    Ok(())
}
