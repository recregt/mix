use async_trait::async_trait;
use mix_core::Step;

use crate::constants::{
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC,
};
use crate::error::{Error, Result};
use crate::util::{
    copy_file, files_match, remove_file, run, systemd_unit_is_active, warn_on_failure, write_file,
};

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
                && systemd_unit_is_active("nix-daemon.socket").await,
        )
    }

    async fn execute(&mut self) -> Result<()> {
        self.written.push((
            NIX_DAEMON_SERVICE_DEST,
            previous_contents(NIX_DAEMON_SERVICE_DEST).await,
        ));
        copy_file(NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_DEST).await?;

        self.written.push((
            NIX_DAEMON_SOCKET_DEST,
            previous_contents(NIX_DAEMON_SOCKET_DEST).await,
        ));
        copy_file(NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_DEST).await?;

        run("systemctl", &["daemon-reload"]).await?;
        run("systemctl", &["enable", "--now", "nix-daemon.socket"]).await?;
        self.started_socket = true;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        if self.started_socket {
            warn_on_failure(
                "disable nix-daemon.socket",
                run("systemctl", &["disable", "--now", "nix-daemon.socket"]).await,
            );
            self.started_socket = false;
        }

        for (path, previous) in self.written.drain(..).rev() {
            warn_on_failure(
                "restore systemd unit file",
                match previous {
                    Some(contents) => write_file(path, contents).await,
                    None => remove_file(path).await,
                },
            );
        }

        warn_on_failure("reload systemd", run("systemctl", &["daemon-reload"]).await);
        Ok(())
    }
}

async fn previous_contents(path: &str) -> Option<Vec<u8>> {
    tokio::fs::read(path).await.ok()
}
