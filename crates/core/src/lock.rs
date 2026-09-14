use std::fs::{File, OpenOptions};
use std::path::Path;

use nix::fcntl::{Flock, FlockArg};

use crate::error::{Error, Result};

pub struct LockGuard {
    _flock: Flock<File>,
}

pub fn acquire_exclusive(path: impl AsRef<Path>) -> Result<LockGuard> {
    let path = path.as_ref();
    tracing::debug!("acquiring exclusive lock: {}", path.display());

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

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
