use async_trait::async_trait;
use mix_core::Step;

use crate::constants::{NIX_CONF, NIX_CONF_DEST, PROFILE_SNIPPET, PROFILE_SNIPPET_DEST};
use crate::error::{Error, Result};
use crate::util::{create_dir_all, write_file};

pub struct ConfigureNixConf;

#[async_trait]
impl Step for ConfigureNixConf {
    type Error = Error;

    fn name(&self) -> &'static str {
        "write runtime configuration"
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
        create_dir_all(dir).await?;
    }
    write_file(path, contents).await?;
    Ok(())
}
