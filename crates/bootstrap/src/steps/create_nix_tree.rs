use std::path::Path;

use async_trait::async_trait;
use mix_core::{Result, Step};

use crate::util::{create_dir_all, set_permissions};

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
        "create managed runtime directory tree"
    }

    async fn check(&self) -> Result<bool> {
        Ok(PATHS.iter().all(|path| Path::new(path).is_dir()))
    }

    async fn execute(&mut self) -> Result<()> {
        for path in PATHS {
            create_dir_all(*path).await?;
            set_permissions(*path, 0o755).await?;
        }
        Ok(())
    }
}
