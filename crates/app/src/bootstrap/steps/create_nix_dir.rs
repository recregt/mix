use async_trait::async_trait;
use mix_core::paths::NIX_OWNERSHIP_MARKER;
use mix_core::{CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::fs::{create_dir, dir_has_mode, is_file, remove_dir_all, remove_file, set_mode};
use crate::fs::{exists, write_atomic};

const MODE: u32 = 0o755;

#[derive(Default)]
pub struct CreateNixDir {
    created_dir: bool,
}

#[async_trait]
impl Step for CreateNixDir {
    type Error = Error;

    fn name(&self) -> &'static str {
        "create /nix"
    }

    async fn check(&self) -> Result<bool> {
        if !is_file(NIX_OWNERSHIP_MARKER).await {
            return Ok(false);
        }
        Ok(dir_has_mode("/nix", MODE).await)
    }

    async fn execute(&mut self, _token: &CancellationToken) -> Result<()> {
        self.created_dir = !exists("/nix").await;
        if self.created_dir {
            create_dir("/nix", MODE).await?;
        } else {
            set_mode("/nix", MODE).await?;
        }
        write_atomic(NIX_OWNERSHIP_MARKER, b"").await?;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        if self.created_dir {
            remove_dir_all("/nix").await?;
        } else {
            remove_file(NIX_OWNERSHIP_MARKER).await?;
        }
        Ok(())
    }
}
