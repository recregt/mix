use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;

const ROOT: Owner = (0, 0);

#[derive(Default)]
struct Fake {
    spots: BTreeMap<PathBuf, Spot>,
}

impl Fake {
    fn with(mut self, path: &str, spot: Spot) -> Self {
        self.spots.insert(PathBuf::from(path), spot);
        self
    }
}

impl Ground for Fake {
    type Handle = ();

    fn spot(&self, path: &Path) -> (Spot, Option<()>) {
        (self.spots.get(path).cloned().unwrap_or(Spot::Missing), None)
    }

    fn running(&self, _: &Path) -> Owner {
        ROOT
    }
}

fn id(ino: u64) -> FileId {
    FileId {
        dev: 1,
        ino,
        born: None,
    }
}

fn present(kind: Kind, ino: u64) -> Spot {
    Spot::Present(Node {
        kind,
        id: id(ino),
        mode: 0o755,
        owner: ROOT,
    })
}

fn blocked(path: &str, kind: ErrorKind) -> Spot {
    Spot::Blocked(Failure::Io {
        path: PathBuf::from(path),
        kind,
    })
}

fn verdict(action: &Action, ground: &Fake) -> Result<Verdict, Failure> {
    precondition(action, ground, &mut Vec::new())
}

fn io_kind(result: Result<Verdict, Failure>) -> Option<ErrorKind> {
    match result {
        Err(Failure::Io { kind, .. }) => Some(kind),
        _ => None,
    }
}

#[test]
fn a_copy_names_its_missing_source_before_a_taken_destination() {
    let ground = Fake::default().with("/to", present(Kind::Directory, 1));
    let copy = Action::CopyTree {
        from: PathBuf::from("/from"),
        to: PathBuf::from("/to"),
        owner: ROOT,
        mode: 0o755,
    };

    assert_eq!(io_kind(verdict(&copy, &ground)), Some(ErrorKind::NotFound));
}

#[test]
fn a_restore_names_its_missing_backup_before_a_taken_path() {
    let ground = Fake::default().with("/c/c", present(Kind::Directory, 1));
    let restore = Action::Restore {
        path: PathBuf::from("/c/c"),
        from: PathBuf::from("/c/.c.mix-backup-x-1"),
        expect: Expect::Absent,
    };

    assert_eq!(
        io_kind(verdict(&restore, &ground)),
        Some(ErrorKind::NotFound)
    );
}

#[test]
fn removing_what_is_already_gone_is_done_but_a_file_on_the_way_is_not() {
    let gone = Action::RemoveCreated {
        path: PathBuf::from("/a/b"),
        expect: id(7),
    };
    let missing_parent = Fake::default().with("/a/b", blocked("/a/b", ErrorKind::NotFound));
    let file_parent = Fake::default().with("/a/b", blocked("/a/b", ErrorKind::NotADirectory));

    assert_eq!(verdict(&gone, &Fake::default()), Ok(Verdict::Done));
    assert_eq!(verdict(&gone, &missing_parent), Ok(Verdict::Done));
    assert_eq!(
        io_kind(verdict(&gone, &file_parent)),
        Some(ErrorKind::NotADirectory)
    );
}

#[test]
fn setting_aside_a_replaced_object_is_a_conflict() {
    let ground = Fake::default().with("/a", present(Kind::File, 2));
    let aside = Action::SetAside {
        path: PathBuf::from("/a"),
        expect: id(1),
    };

    assert!(matches!(
        verdict(&aside, &ground),
        Err(Failure::Conflict { .. })
    ));
}

#[test]
fn created_directories_start_below_the_nearest_existing_one() {
    let ground = Fake::default()
        .with("/a", present(Kind::Directory, 1))
        .with("/a/b/c", blocked("/a/b/c", ErrorKind::NotFound));
    let create = |path: &str| Action::CreateDirs {
        path: PathBuf::from(path),
        mode: 0o755,
        owner: None,
    };

    assert_eq!(
        verdict(&create("/a/b/c"), &ground),
        Ok(Verdict::Below(PathBuf::from("/a/b")))
    );
    assert_eq!(verdict(&create("/a"), &ground), Ok(Verdict::Done));
}

#[test]
fn a_mode_is_never_set_through_a_symbolic_link() {
    let ground = Fake::default().with("/a", present(Kind::Symlink, 1));
    let set = Action::SetMode {
        path: PathBuf::from("/a"),
        mode: 0o700,
        expect: 0o755,
    };

    assert!(matches!(
        verdict(&set, &ground),
        Err(Failure::Conflict { .. })
    ));
}

#[test]
fn replacing_a_file_needs_the_object_that_was_seen() {
    let ground = Fake::default().with("/a", present(Kind::File, 1));
    let put = |expect| Action::PutFile {
        path: PathBuf::from("/a"),
        contents: Arc::from(&b"x"[..]),
        mode: 0o644,
        owner: None,
        expect,
    };

    assert_eq!(
        verdict(&put(Expect::Present(id(1))), &ground),
        Ok(Verdict::Go)
    );
    assert!(verdict(&put(Expect::Absent), &ground).is_err());
    assert!(verdict(&put(Expect::Present(id(2))), &ground).is_err());
}
