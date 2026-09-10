use std::path::{Path, PathBuf};

use async_trait::async_trait;
use mix_core::{Error, Result, Step};

use crate::tarball;

pub struct FetchAndUnpack;

#[async_trait]
impl Step for FetchAndUnpack {
    fn name(&self) -> &'static str {
        "fetch and unpack Nix"
    }

    async fn check(&self) -> Result<bool> {
        Ok(Path::new("/nix/store").is_dir())
    }

    async fn execute(&mut self) -> Result<()> {
        let bytes = tarball::bytes().await?;
        let dest: PathBuf = "/nix".into();

        tokio::task::spawn_blocking(move || tarball::unpack(&bytes, &dest))
            .await
            .map_err(|e| Error::Other(format!("unpack task panicked: {e}")))??;

        Ok(())
    }
}
