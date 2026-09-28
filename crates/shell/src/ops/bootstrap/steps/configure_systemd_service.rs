use async_trait::async_trait;
use mix_core::{Scope, Step};

use mix_core::paths::{
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC,
};

use crate::effect::exec::run;
use crate::effect::fs::{copy_atomic, files_match, remove_file, write_atomic};
use crate::effect::systemd::unit_is_active;
use crate::ops::bootstrap::cleanup::warn_on_failure;
use crate::ops::bootstrap::error::{Error, Result};

#[derive(Default)]
pub struct ConfigureSystemdService {
    written: Vec<(&'static str, Option<Vec<u8>>)>,
    started_socket: bool,
}

#[async_trait]
impl Step for ConfigureSystemdService {
    type Error = Error;

    fn name(&self) -> &'static str {
        "configure the managed background service"
    }

    async fn check(&self, scope: &Scope) -> Result<bool> {
        Ok(
            files_match(NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_DEST).await
                && files_match(NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_DEST).await
                && unit_is_active("nix-daemon.socket", scope).await,
        )
    }

    async fn execute(&mut self, scope: &Scope) -> Result<()> {
        self.written.push((
            NIX_DAEMON_SERVICE_DEST,
            previous_contents(NIX_DAEMON_SERVICE_DEST).await,
        ));
        copy_atomic(NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_DEST).await?;

        self.written.push((
            NIX_DAEMON_SOCKET_DEST,
            previous_contents(NIX_DAEMON_SOCKET_DEST).await,
        ));
        copy_atomic(NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_DEST).await?;

        run("systemctl", &["daemon-reload"], scope).await?;
        self.started_socket = true;
        run(
            "systemctl",
            &["enable", "--now", "nix-daemon.socket"],
            scope,
        )
        .await?;
        Ok(())
    }

    async fn rollback(&mut self, scope: &Scope) -> Result<()> {
        if self.started_socket {
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
            self.started_socket = false;
        }

        for (path, previous) in self.written.drain(..).rev() {
            warn_on_failure(
                "restore systemd unit file",
                match previous {
                    Some(contents) => write_atomic(path, contents).await,
                    None => remove_file(path).await,
                },
            );
        }

        warn_on_failure(
            "reload systemd",
            run("systemctl", &["daemon-reload"], scope).await,
        );
        Ok(())
    }
}

async fn previous_contents(path: &str) -> Option<Vec<u8>> {
    tokio::fs::read(path).await.ok()
}
