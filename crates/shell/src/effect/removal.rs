use std::io::ErrorKind;
use std::path::Path;

use mix_core::declared::paths::{INSTALLED_PROGRAMS, JOURNAL_DIR, LOCK_FILE, MIX_VAR_DIR};
use mix_core::effect::Failure;

const IF_EMPTY: [&str; 2] = ["/nix", "/etc/nix"];

pub fn leftovers() -> impl Iterator<Item = &'static Path> {
    [LOCK_FILE, JOURNAL_DIR, MIX_VAR_DIR]
        .into_iter()
        .chain(IF_EMPTY)
        .chain(INSTALLED_PROGRAMS)
        .map(Path::new)
}

pub fn finish() -> Vec<Failure> {
    finish_under(Path::new("/"))
}

#[allow(clippy::disallowed_methods)]
fn finish_under(root: &Path) -> Vec<Failure> {
    let at = |path: &str| root.join(path.trim_start_matches('/'));
    let mut left = Vec::new();
    let mut gone = |path: &Path, result: std::io::Result<()>, kept: &[ErrorKind]| match result {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound || kept.contains(&error.kind()) => {}
        Err(error) => left.push(Failure::Io {
            path: path.into(),
            kind: error.kind(),
        }),
    };
    let lock = at(LOCK_FILE);
    gone(&lock, std::fs::remove_file(&lock), &[]);
    let journals = at(JOURNAL_DIR);
    if let Ok(entries) = std::fs::read_dir(&journals) {
        for journal in entries.flatten() {
            let path = journal.path();
            gone(&path, std::fs::remove_file(&path), &[]);
        }
    }
    gone(&journals, std::fs::remove_dir(&journals), &[]);
    let var = at(MIX_VAR_DIR);
    gone(&var, std::fs::remove_dir(&var), &[]);
    for dir in IF_EMPTY {
        let dir = at(dir);
        gone(
            &dir,
            std::fs::remove_dir(&dir),
            &[ErrorKind::DirectoryNotEmpty, ErrorKind::ResourceBusy],
        );
    }
    for program in INSTALLED_PROGRAMS {
        let program = at(program);
        gone(&program, std::fs::remove_file(&program), &[]);
    }
    left
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn under(root: &Path, path: &str) -> std::path::PathBuf {
        root.join(path.trim_start_matches('/'))
    }

    #[test]
    fn finishing_a_purge_removes_every_journal_and_keeps_a_store_someone_else_filled() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let journals = under(root, JOURNAL_DIR);
        std::fs::create_dir_all(&journals).unwrap();
        std::fs::write(journals.join("interrupted.ndjson"), "{}").unwrap();
        std::fs::write(journals.join("another.ndjson"), "{}").unwrap();
        std::fs::write(under(root, LOCK_FILE), "").unwrap();
        std::fs::create_dir_all(under(root, "/nix/kept")).unwrap();
        for program in INSTALLED_PROGRAMS {
            let program = under(root, program);
            std::fs::create_dir_all(program.parent().unwrap()).unwrap();
            std::fs::write(program, "mix").unwrap();
        }

        let left = finish_under(root);

        assert!(left.is_empty(), "{left:?}");
        assert!(!under(root, MIX_VAR_DIR).exists());
        for program in INSTALLED_PROGRAMS {
            assert!(!under(root, program).exists(), "{program}");
        }
        assert!(under(root, "/nix/kept").exists());
    }
}
