use std::fs::{File, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use nix::fcntl::{Flock, FlockArg};
use nix::unistd::{Gid, Uid, chown};

use crate::error::{Error, Result};
use crate::identity::MIX_USERS_GID;
use crate::privilege::is_root;

pub struct LockGuard {
    _flock: Flock<File>,
}

const SHARED_LOCK_MODE: u32 = 0o664;

pub fn acquire_exclusive(path: impl AsRef<Path>) -> Result<LockGuard> {
    let path = path.as_ref();
    tracing::debug!("acquiring exclusive lock: {}", path.display());

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let created = !path.exists();

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

    if created && is_root() {
        share_lock_with_managed_users(path);
    }

    Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map(|flock| LockGuard { _flock: flock })
        .map_err(|(_, errno)| {
            if errno == nix::errno::Errno::EWOULDBLOCK {
                Error::Locked {
                    path: path.to_path_buf(),
                }
            } else {
                Error::Io {
                    path: path.to_path_buf(),
                    source: std::io::Error::from(errno),
                }
            }
        })
}

fn share_lock_with_managed_users(path: &Path) {
    if let Err(e) = chown(
        path,
        Some(Uid::from_raw(0)),
        Some(Gid::from_raw(MIX_USERS_GID)),
    ) {
        tracing::warn!(
            "chown {} to mix-users failed: {e}, continuing",
            path.display()
        );
        return;
    }
    if let Err(e) =
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(SHARED_LOCK_MODE))
    {
        tracing::warn!("chmod {} failed: {e}, continuing", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_exclusive_creates_the_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mix.lock");

        let _guard = acquire_exclusive(&path).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn acquire_exclusive_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("mix.lock");

        let _guard = acquire_exclusive(&path).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn acquire_exclusive_fails_while_another_holder_has_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mix.lock");

        let _held = acquire_exclusive(&path).unwrap();

        assert!(matches!(
            acquire_exclusive(&path),
            Err(Error::Locked { .. })
        ));
    }

    #[test]
    fn acquire_exclusive_succeeds_again_once_the_holder_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mix.lock");

        let held = acquire_exclusive(&path).unwrap();
        drop(held);

        assert!(acquire_exclusive(&path).is_ok());
    }
}
