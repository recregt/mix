use async_trait::async_trait;
use mix_core::{Error, Result, Step};

use crate::constants::{NIX_CONF, NIX_CONF_DEST, PROFILE_SNIPPET, PROFILE_SNIPPET_DEST};

pub struct ConfigureNixConf;

#[async_trait]
impl Step for ConfigureNixConf {
    fn name(&self) -> &'static str {
        "write nix.conf and shell profile snippet"
    }

    async fn check(&self) -> Result<bool> {
        Ok(matches_expected(NIX_CONF_DEST, NIX_CONF).await
            && matches_expected(PROFILE_SNIPPET_DEST, PROFILE_SNIPPET).await)
    }

    async fn execute(&mut self) -> Result<()> {
        write(NIX_CONF_DEST, NIX_CONF).await?;
        write(PROFILE_SNIPPET_DEST, PROFILE_SNIPPET).await?;
        Ok(())
    }
}

async fn matches_expected(path: &str, expected: &str) -> bool {
    tokio::fs::read_to_string(path)
        .await
        .map(|s| s == expected)
        .unwrap_or(false)
}

async fn write(path: &str, contents: &str) -> Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| Error::Io {
                path: dir.to_path_buf(),
                source: e,
            })?;
    }
    tokio::fs::write(path, contents)
        .await
        .map_err(|e| Error::Io {
            path: path.into(),
            source: e,
        })?;
    Ok(())
}
