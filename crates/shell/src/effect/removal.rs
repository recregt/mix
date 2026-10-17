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

#[allow(clippy::disallowed_methods)]
pub fn finish() -> Vec<Failure> {
    let mut left = Vec::new();
    let mut gone = |path: &Path, result: std::io::Result<()>, kept: &[ErrorKind]| match result {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound || kept.contains(&error.kind()) => {}
        Err(error) => left.push(Failure::Io {
            path: path.into(),
            kind: error.kind(),
        }),
    };
    gone(Path::new(LOCK_FILE), std::fs::remove_file(LOCK_FILE), &[]);
    if let Ok(journals) = std::fs::read_dir(JOURNAL_DIR) {
        for journal in journals.flatten() {
            let path = journal.path();
            gone(&path, std::fs::remove_file(&path), &[]);
        }
    }
    gone(
        Path::new(JOURNAL_DIR),
        std::fs::remove_dir(JOURNAL_DIR),
        &[],
    );
    gone(
        Path::new(MIX_VAR_DIR),
        std::fs::remove_dir(MIX_VAR_DIR),
        &[],
    );
    for dir in IF_EMPTY {
        gone(
            Path::new(dir),
            std::fs::remove_dir(dir),
            &[ErrorKind::DirectoryNotEmpty, ErrorKind::ResourceBusy],
        );
    }
    for program in INSTALLED_PROGRAMS {
        gone(Path::new(program), std::fs::remove_file(program), &[]);
    }
    left
}
