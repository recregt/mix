//! Reading and writing the files and directories `mix` declares.
//!
//! Every write here is one a reader could be interrupted in the middle of, so the ones that
//! replace a file do it by rename: a destination is either what it was or what it is being made
//! into, never half of either.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use mix_core::{Error, Result};
use nix::fcntl::{AT_FDCWD, AtFlags};
use nix::unistd::{Gid, Uid, fchownat};
use tokio::io::AsyncWriteExt;

/// Who a declared file or directory belongs to, when mix declares an owner for it at all.
pub type Owner = Option<(u32, u32)>;

/// The bits of a mode that are compared against the one mix declares.
pub(crate) const DIR_MODE_MASK: u32 = 0o7777;

/// The mode given to a directory created only to hold the one that was asked for.
pub(crate) const INTERMEDIATE_DIR_MODE: u32 = 0o755;

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

pub async fn dir_has_mode(path: impl AsRef<Path>, mode: u32) -> bool {
    match tokio::fs::metadata(path.as_ref()).await {
        Ok(meta) => meta.is_dir() && meta.permissions().mode() & DIR_MODE_MASK == mode,
        Err(_) => false,
    }
}

pub async fn write_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
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
        .map_err(|e| io_error(path, e))?;
    guard.disarm();

    sync_dir(dir).await;
    Ok(())
}

pub async fn copy_atomic(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();
    tracing::debug!(
        "copying file atomically: {} -> {}",
        src.display(),
        dest.display()
    );
    let contents = tokio::fs::read(src).await.map_err(|e| io_error(src, e))?;
    write_atomic(dest, contents).await
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

/// Flushes a directory entry so a rename into it survives a power loss.
pub(crate) async fn sync_dir(dir: &Path) {
    if let Ok(handle) = tokio::fs::File::open(dir).await {
        let _ = handle.sync_all().await;
    }
}

async fn write_and_sync(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| io_error(path, e))?;
    file.write_all(contents)
        .await
        .map_err(|e| io_error(path, e))?;
    file.sync_all().await.map_err(|e| io_error(path, e))
}

/// Writes a declared file, creating the directory it belongs in if it is not there yet.
pub async fn write(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_dir_all(parent).await?;
    }
    write_atomic(path, contents).await
}

pub async fn files_match(a: &str, b: &str) -> bool {
    let (a, b) = (tokio::fs::read(a).await, tokio::fs::read(b).await);
    matches!((a, b), (Ok(a), Ok(b)) if a == b)
}

pub async fn create_dir(path: impl AsRef<Path>, mode: u32) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!(
        "creating directory with mode: {} ({mode:o})",
        path.display()
    );

    tokio::fs::DirBuilder::new()
        .mode(mode)
        .create(path)
        .await
        .map_err(|e| io_error(path, e))?;

    // A umask the process did not set is still applied to the mode a directory is created with.
    if !dir_has_mode(path, mode).await {
        set_mode(path, mode).await?;
    }

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_dir(parent).await;
    }
    Ok(())
}

pub async fn create_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("creating directory: {}", path.display());
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| io_error(path, e))
}

/// Creates `path` and every directory it needs, giving each of them an owner.
///
/// The directories that only exist to hold the one that was asked for get a mode that can be
/// traversed; the leaf gets the mode it was declared with.
pub async fn create_dir_all_owned(path: &Path, mode: u32, owner: Owner) -> Result<()> {
    let mut missing = Vec::new();
    let mut cur = path;
    loop {
        if tokio::fs::metadata(cur).await.is_ok() {
            break;
        }
        missing.push(cur);
        match cur.parent() {
            Some(parent) => cur = parent,
            None => break,
        }
    }

    for dir in missing.into_iter().rev() {
        tracing::debug!("creating directory: {}", dir.display());
        match tokio::fs::create_dir(dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_error(dir, e)),
        }
        let dir_mode = if dir == path {
            mode
        } else {
            INTERMEDIATE_DIR_MODE
        };
        set_mode(dir, dir_mode).await?;
        set_owner(dir, owner).await?;
    }

    Ok(())
}

pub async fn remove_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("removing directory: {}", path.display());
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(path, e)),
    }
}

pub async fn remove_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("removing file: {}", path.display());
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(path, e)),
    }
}

pub async fn set_mode(path: impl AsRef<Path>, mode: u32) -> Result<()> {
    let path = path.as_ref();
    tracing::debug!("setting permissions: {} ({mode:o})", path.display());
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|e| io_error(path, e))
}

/// Gives `path` an owner, if it is declared to have one.
pub async fn set_owner(path: &Path, owner: Owner) -> Result<()> {
    let Some((uid, gid)) = owner else {
        return Ok(());
    };
    chown(path, uid, gid).await
}

/// Gives `path` an owner it does not already have, and says whether it had to.
pub async fn set_owner_if_needed(path: &Path, owner: Owner) -> Result<bool> {
    let Some((uid, gid)) = owner else {
        return Ok(false);
    };
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| io_error(path, e))?;
    if meta.uid() == uid && meta.gid() == gid {
        return Ok(false);
    }
    chown(path, uid, gid).await?;
    Ok(true)
}

async fn chown(path: &Path, uid: u32, gid: u32) -> Result<()> {
    tracing::debug!("setting owner: {} ({uid}:{gid})", path.display());
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        nix::unistd::chown(&path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
            .map_err(|e| io_error(&path, std::io::Error::from(e)))
    })
    .await
    .map_err(|e| Error::TaskPanicked(e.to_string()))?
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
    async fn write_atomic_writes_the_full_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        write_atomic(&path, b"hello").await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn write_atomic_replaces_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");
        std::fs::write(&path, b"old").unwrap();

        write_atomic(&path, b"new").await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[tokio::test]
    async fn write_atomic_leaves_no_temp_file_behind_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        write_atomic(&path, b"hello").await.unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("nix.conf")]);
    }

    #[tokio::test]
    async fn write_atomic_does_not_touch_the_destination_when_the_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-subdir").join("nix.conf");

        assert!(write_atomic(&path, b"hello").await.is_err());

        assert!(!path.exists());
    }

    #[tokio::test]
    async fn write_atomic_temp_names_do_not_collide_under_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nix.conf");

        let tasks: Vec<_> = (0..20)
            .map(|i| {
                let path = path.clone();
                tokio::spawn(async move {
                    write_atomic(&path, format!("content-{i}").into_bytes()).await
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

    #[tokio::test]
    async fn files_match_true_for_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert!(files_match(a.to_str().unwrap(), b.to_str().unwrap()).await);
    }

    #[tokio::test]
    async fn files_match_false_for_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"one").unwrap();
        std::fs::write(&b, b"two").unwrap();
        assert!(!files_match(a.to_str().unwrap(), b.to_str().unwrap()).await);
    }

    #[tokio::test]
    async fn files_match_false_when_one_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"one").unwrap();
        let missing = dir.path().join("missing");
        assert!(!files_match(a.to_str().unwrap(), missing.to_str().unwrap()).await);
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

    #[tokio::test]
    async fn create_dir_ignores_the_process_umask() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sticky");

        let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
        let result = create_dir(&target, 0o1777).await;
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

    #[tokio::test]
    async fn copy_atomic_copies_content_to_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::write(&src, b"unit-file-content").unwrap();

        copy_atomic(&src, &dest).await.unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"unit-file-content");
    }
}
