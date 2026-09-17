use async_trait::async_trait;
use mix_core::{CancellationToken, Step};

use mix_core::paths::{
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC,
};

use crate::bootstrap::cleanup::warn_on_failure;
use crate::bootstrap::error::{Error, Result};
use crate::exec::run;
use crate::fs::{copy_atomic, files_match, remove_file, write_atomic};
use crate::systemd::unit_is_active;

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

    async fn check(&self) -> Result<bool> {
        Ok(
            files_match(NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_DEST).await
                && files_match(NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_DEST).await
                && unit_is_active("nix-daemon.socket").await,
        )
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
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

        run("systemctl", &["daemon-reload"], token).await?;
        self.started_socket = true;
        run(
            "systemctl",
            &["enable", "--now", "nix-daemon.socket"],
            token,
        )
        .await?;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        let token = CancellationToken::new();

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
                    &token,
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
            run("systemctl", &["daemon-reload"], &token).await,
        );
        Ok(())
    }
}

async fn previous_contents(path: &str) -> Option<Vec<u8>> {
    tokio::fs::read(path).await.ok()
}
