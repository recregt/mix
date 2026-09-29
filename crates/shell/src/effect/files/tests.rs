use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use mix_core::action::rollback_order;
use mix_core::model::World;

use super::*;

struct Root {
    dir: tempfile::TempDir,
    files: Files,
}

fn root() -> Root {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["etc", "var"] {
        std::fs::create_dir(dir.path().join(sub)).unwrap();
    }
    let files = Files::open(dir.path(), "r1").unwrap();
    Root { dir, files }
}

impl Root {
    fn real(&self, path: &str) -> PathBuf {
        self.dir.path().join(path.trim_start_matches('/'))
    }

    fn id(&self, path: &str) -> FileId {
        match self.files.observe(&Query::Path(path.into())) {
            Some(Fact::Path(PathFacts { id: Some(id), .. })) => id,
            other => panic!("{path} has no id: {other:?}"),
        }
    }

    fn apply(&mut self, action: Action) -> Outcome {
        self.files
            .perform(&action, &mut |_: &[Action]| Ok(()))
            .expect("a file action")
    }

    fn tree(&self) -> Vec<(PathBuf, bool, u32, Option<Vec<u8>>)> {
        let mut entries: Vec<_> = walk(self.dir.path())
            .into_iter()
            .map(|path| {
                let meta = std::fs::symlink_metadata(&path).unwrap();
                let contents = meta.is_file().then(|| std::fs::read(&path).unwrap());
                (
                    path.strip_prefix(self.dir.path()).unwrap().to_path_buf(),
                    meta.is_dir(),
                    meta.permissions().mode() & 0o7777,
                    contents,
                )
            })
            .collect();
        entries.sort();
        entries
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() && !path.is_symlink() {
            found.extend(walk(&path));
        }
        found.push(path);
    }
    found
}

fn bytes(text: &str) -> Arc<[u8]> {
    Arc::from(text.as_bytes())
}

fn undo(root: &mut Root, journal: &[Vec<Action>]) {
    for action in rollback_order(journal) {
        root.apply(action).expect("the undo applies");
    }
}

#[test]
fn a_created_tree_with_a_file_is_undone_to_nothing() {
    let mut root = root();
    let before = root.tree();

    let journal = [
        Action::CreateDir {
            path: "/nix".into(),
            mode: 0o755,
            owner: None,
        },
        Action::PutFile {
            path: "/nix/.mix-managed".into(),
            contents: bytes(""),
            mode: 0o644,
            owner: None,
            expect: Expect::Absent,
        },
    ]
    .map(|action| root.apply(action).unwrap().undo);
    assert_eq!(
        std::fs::metadata(root.real("/nix"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );
    undo(&mut root, &journal);

    assert_eq!(root.tree(), before);
}

#[test]
fn a_replaced_file_is_swapped_in_atomically_and_swapped_back_by_undo() {
    let mut root = root();
    std::fs::write(root.real("/etc/nix.conf"), "trusted-users = root alice\n").unwrap();
    let before = root.tree();
    let old = root.id("/etc/nix.conf");

    let journal = [root
        .apply(Action::PutFile {
            path: "/etc/nix.conf".into(),
            contents: bytes("trusted-users = root\n"),
            mode: 0o644,
            owner: None,
            expect: Expect::Present(old),
        })
        .unwrap()
        .undo];
    assert_eq!(
        std::fs::read_to_string(root.real("/etc/nix.conf")).unwrap(),
        "trusted-users = root\n"
    );
    assert_eq!(root.files.pending().len(), 1);
    undo(&mut root, &journal);

    assert_eq!(root.tree(), before);
    assert_eq!(root.id("/etc/nix.conf"), old);
    assert!(root.files.pending().is_empty());
}

#[test]
fn a_write_on_stale_facts_changes_nothing() {
    let mut root = root();
    std::fs::write(root.real("/etc/nix.conf"), "theirs").unwrap();
    let before = root.tree();

    let absent = root.apply(Action::PutFile {
        path: "/etc/nix.conf".into(),
        contents: bytes("ours"),
        mode: 0o644,
        owner: None,
        expect: Expect::Absent,
    });
    let stale = root.apply(Action::PutFile {
        path: "/etc/nix.conf".into(),
        contents: bytes("ours"),
        mode: 0o644,
        owner: None,
        expect: Expect::Present(FileId {
            dev: 0,
            ino: 0,
            born: None,
        }),
    });

    assert!(matches!(absent, Err(Failure::Conflict { .. })));
    assert!(matches!(stale, Err(Failure::Conflict { .. })));
    assert_eq!(root.tree(), before);
}

#[test]
fn a_symbolic_link_on_the_way_is_refused() {
    let mut root = root();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), root.real("/home")).unwrap();

    let refused = root.apply(Action::PutFile {
        path: "/home/state".into(),
        contents: bytes("x"),
        mode: 0o644,
        owner: None,
        expect: Expect::Absent,
    });

    assert!(
        matches!(refused, Err(Failure::Conflict { .. })),
        "{refused:?}"
    );
    assert!(
        std::fs::read_dir(elsewhere.path())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn an_undo_never_removes_what_someone_else_put_there() {
    let mut root = root();
    let journal = [root
        .apply(Action::CreateDir {
            path: "/nix".into(),
            mode: 0o755,
            owner: None,
        })
        .unwrap()
        .undo];
    std::fs::write(root.real("/nix/theirs"), "x").unwrap();

    let refused = root.apply(rollback_order(&journal).remove(0));

    assert!(
        matches!(refused, Err(Failure::Conflict { .. })),
        "{refused:?}"
    );
    assert!(root.real("/nix/theirs").exists());
}

#[test]
fn an_undo_of_something_replaced_meanwhile_is_refused() {
    let mut root = root();
    let journal = [root
        .apply(Action::PutFile {
            path: "/etc/profile".into(),
            contents: bytes("ours"),
            mode: 0o644,
            owner: None,
            expect: Expect::Absent,
        })
        .unwrap()
        .undo];
    std::fs::remove_file(root.real("/etc/profile")).unwrap();
    std::fs::write(root.real("/etc/profile"), "theirs").unwrap();

    let refused = root.apply(rollback_order(&journal).remove(0));

    assert!(
        matches!(refused, Err(Failure::Conflict { .. })),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.real("/etc/profile")).unwrap(),
        "theirs"
    );
}

#[test]
fn a_mode_change_is_checked_against_what_was_seen_and_undone() {
    let mut root = root();
    std::fs::set_permissions(root.real("/var"), std::fs::Permissions::from_mode(0o700)).unwrap();

    let stale = root.apply(Action::SetMode {
        path: "/var".into(),
        mode: 0o755,
        expect: 0o755,
    });
    let journal = [root
        .apply(Action::SetMode {
            path: "/var".into(),
            mode: 0o755,
            expect: 0o700,
        })
        .unwrap()
        .undo];
    undo(&mut root, &journal);

    assert!(matches!(stale, Err(Failure::Conflict { .. })));
    assert_eq!(
        std::fs::metadata(root.real("/var"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
}

#[test]
fn a_tree_set_aside_is_deleted_by_commit_or_brought_back_by_undo() {
    for commit in [true, false] {
        let mut root = root();
        std::fs::create_dir_all(root.real("/nix/store/abc")).unwrap();
        std::fs::write(root.real("/nix/store/abc/bin"), "x").unwrap();
        let before = root.tree();
        let id = root.id("/nix");

        let journal = [root
            .apply(Action::SetAside {
                path: "/nix".into(),
                expect: id,
            })
            .unwrap()
            .undo];
        assert!(!root.real("/nix").exists());
        if commit {
            root.apply(Action::Commit).unwrap();
            assert_eq!(root.tree().len(), before.len() - 4);
        } else {
            undo(&mut root, &journal);
            assert_eq!(root.tree(), before);
        }
        assert!(root.files.pending().is_empty());
    }
}

#[test]
fn the_file_system_shows_what_the_model_predicts() {
    let mut root = root();
    let mut world = World::default();
    world.with_dir("/var", 0o755, (0, 0));
    std::fs::set_permissions(root.real("/var"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let actions = [
        Action::CreateDir {
            path: "/nix".into(),
            mode: 0o755,
            owner: None,
        },
        Action::CreateDir {
            path: "/nix/var".into(),
            mode: 0o700,
            owner: None,
        },
        Action::PutFile {
            path: "/nix/.mix-managed".into(),
            contents: bytes(""),
            mode: 0o644,
            owner: None,
            expect: Expect::Absent,
        },
        Action::PutFile {
            path: "/etc/nix.conf".into(),
            contents: bytes("trusted-users = root\n"),
            mode: 0o644,
            owner: None,
            expect: Expect::Absent,
        },
        Action::SetMode {
            path: "/var".into(),
            mode: 0o711,
            expect: 0o755,
        },
    ];
    let paths = [
        "/nix",
        "/nix/var",
        "/nix/.mix-managed",
        "/etc/nix.conf",
        "/var",
    ];
    let initial = root.tree();

    let mut journal = Vec::new();
    for action in &actions {
        let predicted = world.apply(action);
        let real = root
            .files
            .perform(action, &mut |_: &[Action]| Ok(()))
            .unwrap();
        assert_eq!(predicted.is_ok(), real.is_ok(), "{action:?}");
        journal.push(real.unwrap().undo);
    }
    let shape = |fact: Option<Fact>| match fact {
        Some(Fact::Path(facts)) => (facts.kind, facts.mode),
        other => panic!("{other:?}"),
    };
    for path in paths {
        assert_eq!(
            shape(Some(world.observe(&Query::Path(path.into())))),
            shape(root.files.observe(&Query::Path(path.into()))),
            "{path}"
        );
        assert_eq!(
            world.observe(&Query::Contents(path.into())),
            root.files.observe(&Query::Contents(path.into())).unwrap(),
            "{path}"
        );
    }
    undo(&mut root, &journal);
    assert_eq!(root.tree(), initial);
}

#[test]
fn a_created_tree_is_removed_whole_but_only_if_it_is_still_the_one_made() {
    let mut root = root();
    std::fs::create_dir_all(root.real("/var/.git/objects")).unwrap();
    std::fs::write(root.real("/var/.git/HEAD"), "ref").unwrap();
    let id = root.id("/var/.git");

    let stale = root.apply(Action::RemoveCreatedTree {
        path: "/var/.git".into(),
        expect: FileId {
            dev: id.dev,
            ino: id.ino + 1,
            born: id.born,
        },
    });
    assert!(matches!(stale, Err(Failure::Conflict { .. })));
    assert!(root.real("/var/.git/HEAD").exists());

    root.apply(Action::RemoveCreatedTree {
        path: "/var/.git".into(),
        expect: id,
    })
    .unwrap();

    assert!(!root.real("/var/.git").exists());
    assert!(
        root.tree()
            .iter()
            .all(|(path, ..)| !path.to_string_lossy().contains("mix-remove"))
    );
}

#[test]
fn reading_follows_links_inside_the_root_but_writing_never_does() {
    let mut root = root();
    std::fs::create_dir_all(root.real("/nix/store/abc-nix/bin")).unwrap();
    std::fs::write(root.real("/nix/store/abc-nix/bin/nix-env"), "binary").unwrap();
    std::os::unix::fs::symlink("/nix/store/abc-nix", root.real("/nix/profile")).unwrap();

    assert_eq!(
        root.files
            .observe(&Query::Contents("/nix/profile/bin/nix-env".into())),
        Some(Fact::Contents(Some(Arc::from(&b"binary"[..]))))
    );
    assert!(matches!(
        root.files.observe(&Query::Path("/nix/profile".into())),
        Some(Fact::Path(PathFacts {
            kind: Kind::Symlink,
            ..
        }))
    ));
    let refused = root.apply(Action::PutFile {
        path: "/nix/profile/bin/other".into(),
        contents: bytes("x"),
        mode: 0o644,
        owner: None,
        expect: Expect::Absent,
    });
    assert!(
        matches!(refused, Err(Failure::Conflict { .. })),
        "{refused:?}"
    );
}
