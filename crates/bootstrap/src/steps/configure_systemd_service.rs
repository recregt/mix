use async_trait::async_trait;
use mix_core::{Result, Step};

use crate::constants::{
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC,
};
use crate::util::{copy_file, files_match, run, systemd_unit_is_active};

pub struct ConfigureSystemdService;

#[async_trait]
impl Step for ConfigureSystemdService {
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
        copy_file(NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_DEST).await?;
        copy_file(NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_DEST).await?;
        run("systemctl", &["daemon-reload"]).await?;
        run("systemctl", &["enable", "--now", "nix-daemon.socket"]).await?;
        Ok(())
    }
}
