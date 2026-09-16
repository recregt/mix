use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use mix_core::{Error, Result};
use nix::fcntl::{AT_FDCWD, AtFlags};
use nix::unistd::{Gid, Uid, fchownat};
use tokio::io::AsyncWriteExt;

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

pub async fn write_file_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("writing file atomically: {}", path.display());

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp_path = dir.join(format!(
        ".{file_name}.mix-tmp-{}-{}",
        std::process::id(),
        next_temp_nonce()
    ));

    let guard = TempFileGuard::new(temp_path.clone());
    write_and_sync(&temp_path, contents.as_ref()).await?;

    tokio::fs::rename(&temp_path, path)
        .await
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    guard.disarm();

    sync_dir_best_effort(dir).await;
    Ok(())
}

fn next_temp_nonce() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

struct TempFileGuard {
    path: std::path::PathBuf,
    armed: bool,
}

impl TempFileGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn sync_dir_best_effort(dir: &Path) {
    if let Ok(handle) = tokio::fs::File::open(dir).await {
        let _ = handle.sync_all().await;
    }
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

async fn write_and_sync(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = tokio::fs::File::create(path).await.map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    file.write_all(contents).await.map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    file.sync_all().await.map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })
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

pub const DIR_MODE_MASK: u32 = 0o7777;

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

    #[tokio::test]
    async fn write_file_atomic_writes_the_full_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        write_file_atomic(&path, b"hello").await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn write_file_atomic_replaces_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");
        std::fs::write(&path, b"old").unwrap();

        write_file_atomic(&path, b"new").await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[tokio::test]
    async fn write_file_atomic_leaves_no_temp_file_behind_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        write_file_atomic(&path, b"hello").await.unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("nix.conf")]);
    }

    #[tokio::test]
    async fn write_file_atomic_does_not_touch_the_destination_when_the_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-subdir").join("nix.conf");

        assert!(write_file_atomic(&path, b"hello").await.is_err());

        assert!(!path.exists());
    }

    #[tokio::test]
    async fn write_file_atomic_temp_names_do_not_collide_under_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        let tasks: Vec<_> = (0..20)
            .map(|i| {
                let path = path.clone();
                tokio::spawn(async move {
                    write_file_atomic(&path, format!("content-{i}").into_bytes()).await
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }

        assert!(path.exists());
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("nix.conf")],
            "no orphaned or colliding temp file should remain"
        );
    }

    #[test]
    fn temp_file_guard_removes_the_file_when_dropped_while_armed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leftover");
        std::fs::write(&path, b"x").unwrap();

        drop(TempFileGuard::new(path.clone()));

        assert!(!path.exists());
    }

    #[test]
    fn temp_file_guard_leaves_the_file_when_disarmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kept");
        std::fs::write(&path, b"x").unwrap();

        TempFileGuard::new(path.clone()).disarm();

        assert!(path.exists());
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
