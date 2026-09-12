use async_trait::async_trait;
use mix_core::Step;

use crate::constants::{NIX_CONF, NIX_CONF_DEST, PROFILE_SNIPPET, PROFILE_SNIPPET_DEST};
use crate::error::{Error, Result};
use crate::util::{create_dir_all, remove_file, write_file};

#[derive(Default)]
pub struct ConfigureNixConf {
    written: Vec<(&'static str, Option<Vec<u8>>)>,
}

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
        self.written
            .push((NIX_CONF_DEST, previous_contents(NIX_CONF_DEST).await));
        write(NIX_CONF_DEST, NIX_CONF).await?;

        self.written.push((
            PROFILE_SNIPPET_DEST,
            previous_contents(PROFILE_SNIPPET_DEST).await,
        ));
        write(PROFILE_SNIPPET_DEST, PROFILE_SNIPPET).await?;

        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        for (path, previous) in self.written.drain(..).rev() {
            match previous {
                Some(contents) => write_file(path, contents).await?,
                None => remove_file(path).await?,
            }
        }
        Ok(())
    }
}

async fn matches_expected(path: &str, expected: &str) -> bool {
    tokio::fs::read_to_string(path)
        .await
        .map(|s| s == expected)
        .unwrap_or(false)
}

async fn previous_contents(path: &str) -> Option<Vec<u8>> {
    tokio::fs::read(path).await.ok()
}

async fn write(path: &str, contents: &str) -> Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        create_dir_all(dir).await?;
    }
    write_file(path, contents).await?;
    Ok(())
}
