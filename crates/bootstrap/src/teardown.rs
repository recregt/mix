use mix_core::Result;

use crate::constants::{
    NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SOCKET_DEST, NIXBLD_GROUP,
    NIXBLD_USER_COUNT, PROFILE_SNIPPET_DEST,
};
use crate::steps::create_users_and_groups::{group_exists, user_exists, user_name};
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
        remove_file_if_present(NIX_DAEMON_SOCKET_DEST).await,
    );
    warn_on_failure(
        "remove nix-daemon.service unit",
        remove_file_if_present(NIX_DAEMON_SERVICE_DEST).await,
    );
    warn_on_failure("reload systemd", run("systemctl", &["daemon-reload"]).await);

    for n in 1..=NIXBLD_USER_COUNT {
        let name = user_name(n);
        if user_exists(&name) {
            warn_on_failure("delete build user", run("userdel", &[&name]).await);
        }
    }
    if group_exists(NIXBLD_GROUP) {
        warn_on_failure(
            "delete nixbld group",
            run("groupdel", &[NIXBLD_GROUP]).await,
        );
    }

    warn_on_failure(
        "remove nix.conf",
        remove_file_if_present(NIX_CONF_DEST).await,
    );
    warn_on_failure(
        "remove profile snippet",
        remove_file_if_present(PROFILE_SNIPPET_DEST).await,
    );

    Ok(())
}

async fn remove_file_if_present(path: &str) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn warn_on_failure<T, E: std::fmt::Display>(
    action: &'static str,
    result: std::result::Result<T, E>,
) {
    if let Err(error) = result {
        tracing::warn!("teardown step failed ({action}): {error}, continuing");
    }
}
