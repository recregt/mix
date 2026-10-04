//! Whether an installed program is the one that is running.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use mix_core::action::ProgramFacts;

/// Compares the installed program at `path` with the one at `source`.
///
/// Note: The same file (device and inode) is the same program without reading either; only
/// different files are compared byte for byte, and only then is `source` kept for a fix.
pub fn observe(path: &Path, source: &Path) -> ProgramFacts {
    let installed = std::fs::symlink_metadata(path).ok();
    let running = std::fs::metadata(source).ok();
    if let (Some(installed), Some(running)) = (&installed, &running)
        && installed.file_type().is_file()
        && (installed.dev(), installed.ino()) == (running.dev(), running.ino())
    {
        return ProgramFacts {
            same: true,
            source: None,
        };
    }
    let Ok(source) = std::fs::read(source) else {
        return ProgramFacts {
            same: false,
            source: None,
        };
    };
    let same = installed.is_some_and(|installed| installed.file_type().is_file())
        && std::fs::read(path).is_ok_and(|installed| installed == source);
    ProgramFacts {
        same,
        source: (!same).then(|| source.into()),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn the_same_file_is_the_same_program_without_being_read() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("program");
        std::fs::write(&program, "binary").unwrap();
        std::fs::hard_link(&program, dir.path().join("running")).unwrap();

        assert_eq!(
            observe(&program, &dir.path().join("running")),
            ProgramFacts {
                same: true,
                source: None
            }
        );
    }

    #[test]
    fn a_copy_with_the_same_bytes_is_the_same_program() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("program"), "binary").unwrap();
        std::fs::write(dir.path().join("running"), "binary").unwrap();

        assert!(observe(&dir.path().join("program"), &dir.path().join("running")).same);
    }

    #[test]
    fn a_different_or_missing_program_carries_the_running_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("program"), "tampered").unwrap();
        std::fs::write(dir.path().join("running"), "binary").unwrap();

        for installed in ["program", "missing"] {
            assert_eq!(
                observe(&dir.path().join(installed), &dir.path().join("running")),
                ProgramFacts {
                    same: false,
                    source: Some(b"binary"[..].into())
                },
                "{installed}"
            );
        }
    }
}
