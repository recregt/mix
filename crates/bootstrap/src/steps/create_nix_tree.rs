use async_trait::async_trait;
use mix_core::{CancellationToken, Step};

use crate::error::{Error, Result};
use crate::util::{create_dir_all, dir_has_mode, remove_dir_all, set_permissions, warn_on_failure};

#[derive(Debug, Clone, Copy)]
pub struct DirectorySpec {
    pub path: &'static str,
    pub mode: u32,
}

pub const NIX_TREE: &[DirectorySpec] = &[
    DirectorySpec {
        path: "/nix/var",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/log",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/log/nix",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/log/nix/drvs",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix/db",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix/gcroots",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix/gcroots/per-user",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix/profiles",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix/profiles/per-user",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix/temproots",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix/userpool",
        mode: 0o755,
    },
    DirectorySpec {
        path: "/nix/var/nix/daemon-socket",
        mode: 0o755,
    },
];

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
        for dir in NIX_TREE {
            if !dir_matches(dir).await {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn execute(&mut self, _token: CancellationToken) -> Result<()> {
        provision_all(NIX_TREE, &mut self.created).await
    }

    async fn rollback(&mut self) -> Result<()> {
        for path in self.created.drain(..).rev() {
            warn_on_failure("remove managed directory", remove_dir_all(path).await);
        }
        Ok(())
    }
}

async fn provision_all(dirs: &[DirectorySpec], created: &mut Vec<&'static str>) -> Result<()> {
    for dir in dirs {
        if !path_exists(dir.path).await {
            created.push(dir.path);
        }
        provision(dir).await?;
    }
    Ok(())
}

async fn provision(dir: &DirectorySpec) -> Result<()> {
    create_dir_all(dir.path).await?;
    set_permissions(dir.path, dir.mode).await?;
    Ok(())
}

async fn path_exists(path: &str) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

async fn dir_matches(dir: &DirectorySpec) -> bool {
    dir_has_mode(dir.path, dir.mode).await
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
        let spec = DirectorySpec {
            path: leak(root.path().join("sticky")),
            mode: 0o1777,
        };

        provision(&spec).await.unwrap();

        assert!(dir_matches(&spec).await);
    }

    #[tokio::test]
    async fn provisioning_repairs_an_existing_directory_with_drifted_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sticky");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let spec = DirectorySpec {
            path: leak(path),
            mode: 0o1777,
        };

        assert!(!dir_matches(&spec).await);
        provision(&spec).await.unwrap();
        assert!(dir_matches(&spec).await);
    }

    #[tokio::test]
    async fn rollback_only_removes_directories_it_created() {
        let root = tempfile::tempdir().unwrap();
        let pre_existing = root.path().join("pre-existing");
        std::fs::create_dir(&pre_existing).unwrap();
        let real_data = pre_existing.join("real-data");
        std::fs::write(&real_data, "do not delete me").unwrap();
        let fresh = root.path().join("fresh");

        let dirs = [
            DirectorySpec {
                path: leak(pre_existing.clone()),
                mode: 0o755,
            },
            DirectorySpec {
                path: leak(fresh.clone()),
                mode: 0o755,
            },
        ];

        let mut created = Vec::new();
        provision_all(&dirs, &mut created).await.unwrap();
        assert_eq!(created, vec![dirs[1].path]);

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
