use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use nix::fcntl::{Flock, FlockArg};

use crate::error::{Error, Result};

pub struct LockGuard {
    _flock: Flock<File>,
}

const LOCK_MODE: u32 = 0o644;
const LOCK_DIR_MODE: u32 = 0o755;

pub fn acquire_exclusive(path: impl AsRef<Path>) -> Result<LockGuard> {
    let path = path.as_ref();
    tracing::debug!("acquiring exclusive lock: {}", path.display());

    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => create(path)?,
        Err(e) => return Err(io_error(path, e)),
    };

    Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map(|flock| LockGuard { _flock: flock })
        .map_err(|(_, errno)| {
            if errno == nix::errno::Errno::EWOULDBLOCK {
                Error::Locked {
                    path: path.to_path_buf(),
                }
            } else {
                io_error(path, std::io::Error::from(errno))
            }
        })
}

fn create(path: &Path) -> Result<File> {
    let refused = |at: &Path, e: std::io::Error| match e.kind() {
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => Error::LockMissing {
            path: path.to_path_buf(),
        },
        _ => io_error(at, e),
    };

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && !parent.exists()
    {
        std::fs::create_dir_all(parent).map_err(|e| refused(parent, e))?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(LOCK_DIR_MODE))
            .map_err(|e| io_error(parent, e))?;
    }

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| refused(path, e))?;
    file.set_permissions(std::fs::Permissions::from_mode(LOCK_MODE))
        .map_err(|e| io_error(path, e))?;
    Ok(file)
}

fn io_error(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
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

    #[test]
    fn a_lock_that_cannot_be_written_is_still_taken_through_a_read_only_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mix.lock");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

        let held = acquire_exclusive(&path).unwrap();

        assert!(matches!(
            acquire_exclusive(&path),
            Err(Error::Locked { .. })
        ));
        drop(held);
        assert!(acquire_exclusive(&path).is_ok());
    }

    #[test]
    fn a_lock_nobody_here_may_create_is_reported_as_missing() {
        if crate::privilege::is_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let path = dir.path().join("mix").join("lock");

        let result = acquire_exclusive(&path);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(matches!(result, Err(Error::LockMissing { .. })));
    }
}
