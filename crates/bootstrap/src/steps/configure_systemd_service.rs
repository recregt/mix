use async_trait::async_trait;
use mix_core::{Error, Result, Step};

use crate::constants::{
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC,
};
use crate::util::{files_match, run};

pub struct ConfigureSystemdService;

#[async_trait]
impl Step for ConfigureSystemdService {
    fn name(&self) -> &'static str {
        "configure the managed background service"
    }

    async fn check(&self) -> Result<bool> {
        Ok(
            files_match(NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_DEST).await
                && files_match(NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_DEST).await,
        )
    }

    async fn execute(&mut self) -> Result<()> {
        copy(NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_DEST).await?;
        copy(NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_DEST).await?;
        run("systemctl", &["daemon-reload"]).await?;
        run("systemctl", &["enable", "--now", "nix-daemon.socket"]).await?;
        Ok(())
    }
}

async fn copy(src: &str, dest: &str) -> Result<()> {
    tokio::fs::copy(src, dest)
        .await
        .map(|_| ())
        .map_err(|e| Error::Io {
            path: dest.into(),
            source: e,
        })
}
