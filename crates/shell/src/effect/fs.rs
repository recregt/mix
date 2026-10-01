//! Looking at the files and directories `mix` declares, and handing a tree to its owner.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use mix_core::{Error, Result};
use nix::fcntl::{AT_FDCWD, AtFlags};
use nix::unistd::{Gid, Uid, fchownat};

pub async fn exists(path: impl AsRef<Path>) -> bool {
    tokio::fs::try_exists(path.as_ref()).await.unwrap_or(false)
}

pub async fn is_file(path: impl AsRef<Path>) -> bool {
    tokio::fs::metadata(path.as_ref())
        .await
        .is_ok_and(|meta| meta.is_file())
}

pub async fn is_dir(path: impl AsRef<Path>) -> bool {
    tokio::fs::metadata(path.as_ref())
        .await
        .is_ok_and(|meta| meta.is_dir())
}

/// Gives everything under `root` the same owner, without following a symlink out of it.
pub(crate) fn chown_tree(root: &Path, uid: Uid, gid: Gid) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).map_err(|e| io_error(&path, e))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let entries = std::fs::read_dir(&path).map_err(|e| io_error(&path, e))?;
            for entry in entries {
                let entry = entry.map_err(|e| io_error(&path, e))?;
                stack.push(entry.path());
            }
        }

        if meta.gid() != gid.as_raw() || meta.uid() != uid.as_raw() {
            fchownat(
                AT_FDCWD,
                &path,
                Some(uid),
                Some(gid),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            )
            .map_err(|e| io_error(&path, std::io::Error::from(e)))?;
        }
    }
    Ok(())
}

pub(crate) fn io_error(path: impl AsRef<Path>, source: std::io::Error) -> Error {
    Error::Io {
        path: PathBuf::from(path.as_ref()),
        source,
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn path_exists_true_for_a_real_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(exists(dir.path()).await);
    }

    #[tokio::test]
    async fn path_exists_false_when_missing() {
        assert!(!exists("/does/not/exist/mix-test").await);
    }

    #[tokio::test]
    async fn is_file_true_for_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(is_file(&file).await);
    }

    #[tokio::test]
    async fn is_file_false_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_file(dir.path()).await);
    }

    #[tokio::test]
    async fn is_file_false_when_missing() {
        assert!(!is_file("/does/not/exist/mix-test").await);
    }

    #[tokio::test]
    async fn is_dir_true_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_dir(dir.path()).await);
    }

    #[tokio::test]
    async fn is_dir_false_for_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(!is_dir(&file).await);
    }

    #[tokio::test]
    async fn is_dir_false_when_missing() {
        assert!(!is_dir("/does/not/exist/mix-test").await);
    }

    #[test]
    fn chown_tree_is_a_noop_when_already_matching() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "x").unwrap();

        let uid = nix::unistd::Uid::current();
        let gid = nix::unistd::Gid::current();

        assert!(chown_tree(dir.path(), uid, gid).is_ok());
    }

    #[test]
    fn chown_tree_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target.path(), dir.path().join("link")).unwrap();

        let uid = nix::unistd::Uid::current();
        let gid = nix::unistd::Gid::current();

        assert!(chown_tree(dir.path(), uid, gid).is_ok());
    }
}
