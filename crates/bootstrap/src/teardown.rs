use mix_core::Result;

use crate::constants::{
    NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SOCKET_DEST, NIXBLD_GROUP,
    NIXBLD_USER_COUNT, PROFILE_SNIPPET_DEST,
};
use crate::steps::create_users_and_groups::user_name;
use crate::util::run;

pub async fn teardown() -> Result<()> {
    warn_on_failure(
        "disable nix-daemon.socket",
        run("systemctl", &["disable", "--now", "nix-daemon.socket"]).await,
    );
    warn_on_failure(
        "disable nix-daemon.service",
        run("systemctl", &["disable", "--now", "nix-daemon.service"]).await,
    );
    warn_on_failure(
        "remove nix-daemon.socket unit",
        tokio::fs::remove_file(NIX_DAEMON_SOCKET_DEST).await,
    );
    warn_on_failure(
        "remove nix-daemon.service unit",
        tokio::fs::remove_file(NIX_DAEMON_SERVICE_DEST).await,
    );
    warn_on_failure("reload systemd", run("systemctl", &["daemon-reload"]).await);

    for n in 1..=NIXBLD_USER_COUNT {
        warn_on_failure("delete build user", run("userdel", &[&user_name(n)]).await);
    }
    warn_on_failure(
        "delete nixbld group",
        run("groupdel", &[NIXBLD_GROUP]).await,
    );

    warn_on_failure(
        "remove nix.conf",
        tokio::fs::remove_file(NIX_CONF_DEST).await,
    );
    warn_on_failure(
        "remove profile snippet",
        tokio::fs::remove_file(PROFILE_SNIPPET_DEST).await,
    );

    Ok(())
}

fn warn_on_failure<T, E: std::fmt::Display>(
    action: &'static str,
    result: std::result::Result<T, E>,
) {
    if let Err(error) = result {
        tracing::warn!("teardown step failed ({action}): {error}, continuing");
    }
}
