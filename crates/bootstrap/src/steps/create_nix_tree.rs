use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use async_trait::async_trait;
use mix_core::{Error, Result, Step};

const PATHS: &[&str] = &[
    "/nix/var",
    "/nix/var/log",
    "/nix/var/log/nix",
    "/nix/var/log/nix/drvs",
    "/nix/var/nix",
    "/nix/var/nix/db",
    "/nix/var/nix/gcroots",
    "/nix/var/nix/gcroots/per-user",
    "/nix/var/nix/profiles",
    "/nix/var/nix/profiles/per-user",
    "/nix/var/nix/temproots",
    "/nix/var/nix/userpool",
    "/nix/var/nix/daemon-socket",
];

pub struct CreateNixTree;

#[async_trait]
impl Step for CreateNixTree {
    fn name(&self) -> &'static str {
        "create the /nix/var directory tree"
    }

    async fn check(&self) -> Result<bool> {
        Ok(PATHS.iter().all(|path| Path::new(path).is_dir()))
    }

    async fn execute(&mut self) -> Result<()> {
        for path in PATHS {
            tokio::fs::create_dir_all(path)
                .await
                .map_err(|e| Error::Io {
                    path: (*path).into(),
                    source: e,
                })?;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .await
                .map_err(|e| Error::Io {
                    path: (*path).into(),
                    source: e,
                })?;
        }
        Ok(())
    }
}
