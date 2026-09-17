use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use mix_core::{Error, Result};
use nix::fcntl::{AT_FDCWD, AtFlags};
use nix::unistd::{Gid, Uid, fchownat};

use crate::shared::os::{DIR_MODE_MASK, sync_dir_best_effort, write_file_atomic};

pub async fn create_dir_with_mode(path: impl AsRef<Path>, mode: u32) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!(
        "creating directory with mode: {} ({mode:o})",
        path.display()
    );

    tokio::fs::DirBuilder::new()
        .mode(mode)
        .create(path)
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

    if !dir_has_mode(path, mode).await {
        set_permissions(path, mode).await?;
    }

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_dir_best_effort(parent).await;
    }
    Ok(())
}

pub async fn create_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("creating directory: {}", path.display());
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })
}

pub async fn copy_file_atomic(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();
    tracing::debug!(
        "copying file atomically: {} -> {}",
        src.display(),
        dest.display()
    );
    let contents = tokio::fs::read(src).await.map_err(|e| Error::Io {
        path: src.to_path_buf(),
        source: e,
    })?;
    write_file_atomic(dest, contents).await
}

pub async fn set_permissions(path: impl AsRef<Path>, mode: u32) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("setting permissions: {} ({mode:o})", path.display());
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })
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

pub async fn dir_has_mode(path: impl AsRef<Path>, mode: u32) -> bool {
    match tokio::fs::metadata(path.as_ref()).await {
        Ok(meta) => meta.is_dir() && meta.permissions().mode() & DIR_MODE_MASK == mode,
        Err(_) => false,
    }
}

pub async fn remove_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("removing directory: {}", path.display());
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

pub async fn remove_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("removing file: {}", path.display());
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

pub(crate) fn ensure_ownership_under(root: &Path, uid: Uid, gid: Gid) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let entries = std::fs::read_dir(&path).map_err(|e| Error::Io {
                path: path.clone(),
                source: e,
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| Error::Io {
                    path: path.clone(),
                    source: e,
                })?;
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
            .map_err(|e| Error::Io {
                path: path.clone(),
                source: std::io::Error::from(e),
            })?;
        }
    }
    Ok(())
}

pub(crate) fn warn_on_failure<T, E: std::fmt::Display>(
    action: &'static str,
    result: std::result::Result<T, E>,
) {
    if let Err(error) = result {
        tracing::warn!("{action} failed: {error}, continuing");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn create_dir_with_mode_ignores_the_process_umask() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sticky");

        let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
        let result = create_dir_with_mode(&target, 0o1777).await;
        nix::sys::stat::umask(previous);

        result.unwrap();
        assert!(dir_has_mode(&target, 0o1777).await);
    }

    #[tokio::test]
    async fn dir_has_mode_true_when_mode_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(dir_has_mode(dir.path(), 0o755).await);
    }

    #[tokio::test]
    async fn dir_has_mode_true_for_a_sticky_world_writable_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(dir_has_mode(dir.path(), 0o1777).await);
    }

    #[tokio::test]
    async fn dir_has_mode_false_on_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!dir_has_mode(dir.path(), 0o755).await);
    }

    #[tokio::test]
    async fn dir_has_mode_false_when_missing() {
        assert!(!dir_has_mode(Path::new("/does/not/exist/mix-test"), 0o755).await);
    }

    #[test]
    fn ensure_ownership_under_is_a_noop_when_already_matching() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "x").unwrap();

        let uid = nix::unistd::Uid::current();
        let gid = nix::unistd::Gid::current();

        assert!(ensure_ownership_under(dir.path(), uid, gid).is_ok());
    }

    #[test]
    fn ensure_ownership_under_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target.path(), dir.path().join("link")).unwrap();

        let uid = nix::unistd::Uid::current();
        let gid = nix::unistd::Gid::current();

        assert!(ensure_ownership_under(dir.path(), uid, gid).is_ok());
    }

    #[tokio::test]
    async fn copy_file_atomic_copies_content_to_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::write(&src, b"unit-file-content").unwrap();

        copy_file_atomic(&src, &dest).await.unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"unit-file-content");
    }
}
