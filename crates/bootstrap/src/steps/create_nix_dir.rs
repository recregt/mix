use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use async_trait::async_trait;
use mix_core::Step;

use crate::constants::NIX_OWNERSHIP_MARKER;
use crate::error::{Error, Result};
use crate::util::{create_dir_all, set_permissions, write_file};

const MODE: u32 = 0o755;

pub struct CreateNixDir;

#[async_trait]
impl Step for CreateNixDir {
    type Error = Error;

    fn name(&self) -> &'static str {
        "create /nix"
    }

    async fn check(&self) -> Result<bool> {
        if !Path::new(NIX_OWNERSHIP_MARKER).is_file() {
            return Ok(false);
        }
        Ok(dir_has_mode(Path::new("/nix"), MODE).await)
    }

    async fn execute(&mut self) -> Result<()> {
        create_dir_all("/nix").await?;
        set_permissions("/nix", MODE).await?;
        write_file(NIX_OWNERSHIP_MARKER, b"").await?;
        Ok(())
    }
}

async fn dir_has_mode(path: &Path, mode: u32) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(meta) => meta.is_dir() && meta.permissions().mode() & 0o777 == mode,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dir_has_mode_true_when_mode_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(dir_has_mode(dir.path(), 0o755).await);
    }

    #[tokio::test]
    async fn dir_has_mode_false_on_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!dir_has_mode(dir.path(), 0o755).await);
    }

    #[tokio::test]
    async fn dir_has_mode_false_when_missing() {
        assert!(!dir_has_mode(Path::new("/does/not/exist/mix-test"), 0o755).await);
    }
}
