use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use async_trait::async_trait;
use mix_core::{Error, Result, Step};

const MODE: u32 = 0o755;

pub struct CreateNixDir;

#[async_trait]
impl Step for CreateNixDir {
    fn name(&self) -> &'static str {
        "create /nix"
    }

    async fn check(&self) -> Result<bool> {
        Ok(Path::new("/nix").is_dir())
    }

    async fn execute(&mut self) -> Result<()> {
        tokio::fs::create_dir_all("/nix")
            .await
            .map_err(|e| Error::Io {
                path: "/nix".into(),
                source: e,
            })?;
        tokio::fs::set_permissions("/nix", std::fs::Permissions::from_mode(MODE))
            .await
            .map_err(|e| Error::Io {
                path: "/nix".into(),
                source: e,
            })?;
        Ok(())
    }
}
