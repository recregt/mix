use async_trait::async_trait;
use mix_core::Step;

use crate::constants::NIX_OWNERSHIP_MARKER;
use crate::error::{Error, Result};
use crate::util::{
    create_dir_all, dir_has_mode, is_file, path_exists, remove_dir_all, remove_file,
    set_permissions, write_file,
};

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

    async fn execute(&mut self) -> Result<()> {
        self.created_dir = !path_exists("/nix").await;
        create_dir_all("/nix").await?;
        set_permissions("/nix", MODE).await?;
        write_file(NIX_OWNERSHIP_MARKER, b"").await?;
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
