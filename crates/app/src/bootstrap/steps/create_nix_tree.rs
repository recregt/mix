use async_trait::async_trait;
use mix_core::paths::{NIX_TREE_MODE, NIX_TREE_PATHS};
use mix_core::{CancellationToken, Step};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::util::{
    create_dir_with_mode, dir_has_mode, path_exists, remove_dir_all, set_permissions,
    warn_on_failure,
};

#[derive(Default)]
pub struct CreateNixTree {
    created: Vec<&'static str>,
}

#[async_trait]
impl Step for CreateNixTree {
    type Error = Error;

    fn name(&self) -> &'static str {
        "create managed runtime directory tree"
    }

    async fn check(&self) -> Result<bool> {
        for &path in NIX_TREE_PATHS {
            if !dir_matches(path, NIX_TREE_MODE).await {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn execute(&mut self, _token: &CancellationToken) -> Result<()> {
        provision_all(NIX_TREE_PATHS, NIX_TREE_MODE, &mut self.created).await
    }

    async fn rollback(&mut self) -> Result<()> {
        for path in self.created.drain(..).rev() {
            warn_on_failure("remove managed directory", remove_dir_all(path).await);
        }
        Ok(())
    }
}

async fn provision_all(
    paths: &[&'static str],
    mode: u32,
    created: &mut Vec<&'static str>,
) -> Result<()> {
    for &path in paths {
        let is_new = !path_exists(path).await;
        if is_new {
            created.push(path);
        }
        provision(path, mode, is_new).await?;
    }
    Ok(())
}

async fn provision(path: &str, mode: u32, is_new: bool) -> Result<()> {
    if is_new {
        create_dir_with_mode(path, mode).await?;
    } else {
        set_permissions(path, mode).await?;
    }
    Ok(())
}

async fn dir_matches(path: &str, mode: u32) -> bool {
    dir_has_mode(path, mode).await
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn leak(path: std::path::PathBuf) -> &'static str {
        Box::leak(path.to_str().unwrap().to_string().into_boxed_str())
    }

    #[tokio::test]
    async fn provisioning_a_fresh_directory_applies_the_requested_mode() {
        let root = tempfile::tempdir().unwrap();
        let path = leak(root.path().join("sticky"));

        provision(path, 0o1777, true).await.unwrap();

        assert!(dir_matches(path, 0o1777).await);
    }

    #[tokio::test]
    async fn provisioning_repairs_an_existing_directory_with_drifted_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sticky");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = leak(path);

        assert!(!dir_matches(path, 0o1777).await);
        provision(path, 0o1777, false).await.unwrap();
        assert!(dir_matches(path, 0o1777).await);
    }

    #[tokio::test]
    async fn rollback_only_removes_directories_it_created() {
        let root = tempfile::tempdir().unwrap();
        let pre_existing = root.path().join("pre-existing");
        std::fs::create_dir(&pre_existing).unwrap();
        let real_data = pre_existing.join("real-data");
        std::fs::write(&real_data, "do not delete me").unwrap();
        let fresh = root.path().join("fresh");

        let paths = [leak(pre_existing.clone()), leak(fresh.clone())];

        let mut created = Vec::new();
        provision_all(&paths, 0o755, &mut created).await.unwrap();
        assert_eq!(created, vec![paths[1]]);

        for path in created.into_iter().rev() {
            warn_on_failure("remove managed directory", remove_dir_all(path).await);
        }

        assert!(
            real_data.exists(),
            "a directory that pre-dated this step must survive rollback"
        );
        assert!(!fresh.exists());
    }
}
