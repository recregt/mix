use std::path::{Path, PathBuf};

use async_trait::async_trait;
use mix_core::{CancellationToken, Step};

use mix_core::models::{NIX_CONF, PROFILE_SNIPPET};
use mix_core::paths::{NIX_CONF_DEST, PROFILE_SNIPPET_DEST};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::util::{
    create_dir_all, remove_dir_all, remove_file, warn_on_failure, write_file_atomic,
};
use crate::shared::os::path_exists;

#[derive(Default)]
pub struct ConfigureNixConf {
    written: Vec<WrittenFile>,
}

struct WrittenFile {
    path: &'static str,
    previous: Option<Vec<u8>>,
    created_dir: Option<PathBuf>,
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

    async fn execute(&mut self, _token: &CancellationToken) -> Result<()> {
        let previous = previous_contents(NIX_CONF_DEST).await;
        let created_dir = write(NIX_CONF_DEST, NIX_CONF).await?;
        self.written.push(WrittenFile {
            path: NIX_CONF_DEST,
            previous,
            created_dir,
        });

        let previous = previous_contents(PROFILE_SNIPPET_DEST).await;
        let created_dir = write(PROFILE_SNIPPET_DEST, PROFILE_SNIPPET).await?;
        self.written.push(WrittenFile {
            path: PROFILE_SNIPPET_DEST,
            previous,
            created_dir,
        });

        Ok(())
    }

    async fn rollback(&mut self) -> Result<()> {
        for written in self.written.drain(..).rev() {
            warn_on_failure(
                "restore runtime configuration file",
                match written.previous {
                    Some(contents) => write_file_atomic(written.path, contents).await,
                    None => remove_file(written.path).await,
                },
            );
            if let Some(dir) = written.created_dir {
                warn_on_failure("remove created directory", remove_dir_all(dir).await);
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

async fn write(path: &str, contents: &str) -> Result<Option<PathBuf>> {
    let created_dir = match Path::new(path).parent() {
        Some(dir) => {
            let created_dir = first_missing_ancestor(dir).await;
            create_dir_all(dir).await?;
            created_dir
        }
        None => None,
    };
    write_file_atomic(path, contents).await?;
    Ok(created_dir)
}

async fn first_missing_ancestor(dir: &Path) -> Option<PathBuf> {
    let mut missing = None;
    let mut candidate = dir;
    while !path_exists(candidate).await {
        missing = Some(candidate.to_path_buf());
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => break,
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_missing_ancestor_is_none_when_the_directory_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(first_missing_ancestor(dir.path()).await, None);
    }

    #[tokio::test]
    async fn first_missing_ancestor_finds_the_topmost_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b").join("c");
        assert_eq!(
            first_missing_ancestor(&target).await,
            Some(dir.path().join("a"))
        );
    }

    #[tokio::test]
    async fn write_creates_missing_parents_and_reports_the_topmost_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("b").join("nix.conf");

        let created_dir = write(file.to_str().unwrap(), "content").await.unwrap();

        assert_eq!(created_dir, Some(dir.path().join("a")));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "content");
    }

    #[tokio::test]
    async fn write_reports_no_created_dir_when_the_parent_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nix.conf");

        let created_dir = write(file.to_str().unwrap(), "content").await.unwrap();

        assert_eq!(created_dir, None);
    }

    #[tokio::test]
    async fn rollback_removes_a_directory_it_created_for_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("nix.conf");
        let created_dir = write(file.to_str().unwrap(), "content").await.unwrap();

        let mut step = ConfigureNixConf {
            written: vec![WrittenFile {
                path: Box::leak(file.to_str().unwrap().to_string().into_boxed_str()),
                previous: None,
                created_dir,
            }],
        };
        step.rollback().await.unwrap();

        assert!(!dir.path().join("a").exists());
    }
}
