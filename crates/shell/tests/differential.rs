#![allow(clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_core::action::{Action, Expect, Fact, FileId, Owner, PathFacts, Query, rollback_order};
use mix_core::model::{Content, World};
use mix_exec::Scope;
use mix_shell::drive::Performer;
use mix_shell::effect::files::Files;

const ROOT_OWNER: Owner = (0, 0);

fn me() -> Owner {
    (
        nix::unistd::geteuid().as_raw(),
        nix::unistd::getegid().as_raw(),
    )
}

#[derive(Clone, Copy)]
enum Op {
    CreateDir(&'static str, u32),
    CreateDirs(&'static str, u32),
    Put(&'static str, &'static str, u32),
    SetMode(&'static str, u32),
    SetAside(&'static str),
    RemoveCreated(&'static str),
    RemoveCreatedTree(&'static str),
    Copy(&'static str, &'static str, u32),
    Reclaim(&'static str, u32),
    Commit,
}

trait Side {
    fn facts(&mut self, path: &str) -> PathFacts;
    fn perform(&mut self, action: &Action) -> Vec<Action>;
    fn entries(&self) -> Vec<Entry>;
}

type Entry = (String, bool, u32, Option<Vec<u8>>, Option<Owner>);

fn action(op: Op, side: &mut dyn Side) -> Action {
    let id = |side: &mut dyn Side, path: &str| -> FileId {
        side.facts(path).id.expect("the path exists")
    };
    match op {
        Op::CreateDir(path, mode) => Action::CreateDir {
            path: path.into(),
            mode,
            owner: None,
        },
        Op::CreateDirs(path, mode) => Action::CreateDirs {
            path: path.into(),
            mode,
            owner: None,
        },
        Op::Put(path, contents, mode) => Action::PutFile {
            path: path.into(),
            contents: Arc::from(contents.as_bytes()),
            mode,
            owner: None,
            expect: side.facts(path).id.map_or(Expect::Absent, Expect::Present),
        },
        Op::SetMode(path, mode) => Action::SetMode {
            path: path.into(),
            mode,
            expect: side.facts(path).mode,
        },
        Op::SetAside(path) => Action::SetAside {
            path: path.into(),
            expect: id(side, path),
        },
        Op::RemoveCreated(path) => Action::RemoveCreated {
            path: path.into(),
            expect: id(side, path),
        },
        Op::RemoveCreatedTree(path) => Action::RemoveCreatedTree {
            path: path.into(),
            expect: id(side, path),
        },
        Op::Copy(from, to, mode) => Action::CopyTree {
            from: from.into(),
            to: to.into(),
            owner: me(),
            mode,
        },
        Op::Reclaim(path, mode) => Action::ReclaimTree {
            path: path.into(),
            expect: id(side, path),
            owner: me(),
            mode,
        },
        Op::Commit => Action::Commit,
    }
}

fn normalized(path: &Path) -> String {
    path.components()
        .map(|component| {
            let name = component.as_os_str().to_string_lossy();
            if name.contains(".mix-") {
                "<kept by mix>".to_string()
            } else {
                name.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

struct Model(World);

impl Side for Model {
    fn facts(&mut self, path: &str) -> PathFacts {
        match self.0.observe(&Query::Path(path.into())) {
            Fact::Path(facts) => facts,
            other => panic!("{other:?}"),
        }
    }

    fn perform(&mut self, action: &Action) -> Vec<Action> {
        self.0
            .apply(action)
            .unwrap_or_else(|failure| panic!("model: {action:?}: {failure:?}"))
            .undo
    }

    fn entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self
            .0
            .files
            .iter()
            .filter(|(path, _)| path.starts_with("/srv"))
            .map(|(path, entry)| {
                let contents = match &entry.content {
                    Content::File(bytes) => Some(bytes.to_vec()),
                    Content::Directory => None,
                };
                (
                    normalized(path),
                    contents.is_none(),
                    entry.mode,
                    contents,
                    (entry.owner != ROOT_OWNER).then_some(entry.owner),
                )
            })
            .collect();
        entries.sort();
        entries
    }
}

struct Real {
    dir: tempfile::TempDir,
    performer: Performer,
    runtime: tokio::runtime::Runtime,
}

impl Real {
    fn walk(&self, dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            found.push(path.clone());
            if path.is_dir() && !path.is_symlink() {
                self.walk(&path, found);
            }
        }
    }
}

impl Side for Real {
    fn facts(&mut self, path: &str) -> PathFacts {
        let facts = self
            .runtime
            .block_on(self.performer.observe(&[Query::Path(path.into())]))
            .unwrap();
        match facts.into_iter().next() {
            Some(Fact::Path(facts)) => facts,
            other => panic!("{other:?}"),
        }
    }

    fn perform(&mut self, action: &Action) -> Vec<Action> {
        let scope = Scope::root();
        let mut progress = |_| {};
        let mut prepared = |_: &[Action]| Ok(());
        self.runtime
            .block_on(
                self.performer
                    .perform(action, &scope, &mut progress, &mut prepared),
            )
            .unwrap_or_else(|failure| panic!("real: {action:?}: {failure:?}"))
            .undo
    }

    fn entries(&self) -> Vec<Entry> {
        let srv = self.dir.path().join("srv");
        let mut found = vec![srv.clone()];
        self.walk(&srv, &mut found);
        let mut entries: Vec<Entry> = found
            .into_iter()
            .map(|path| {
                let meta = std::fs::symlink_metadata(&path).unwrap();
                let contents = meta.is_file().then(|| std::fs::read(&path).unwrap());
                let inside = Path::new("/").join(path.strip_prefix(self.dir.path()).unwrap());
                (
                    normalized(&inside),
                    meta.is_dir(),
                    meta.permissions().mode() & 0o7777,
                    contents,
                    Some((meta.uid(), meta.gid())),
                )
            })
            .collect();
        entries.sort();
        entries
    }
}

fn sides() -> (Model, Real) {
    let mut world = World::default();
    world
        .with_dir("/srv", 0o755, me())
        .with_dir("/srv/state", 0o755, ROOT_OWNER)
        .with_file("/srv/state/flake.nix", b"{ }", 0o644, ROOT_OWNER)
        .with_dir("/srv/state/.git", 0o755, ROOT_OWNER)
        .with_file("/srv/state/.git/HEAD", b"ref", 0o444, ROOT_OWNER);
    let dir = tempfile::tempdir().unwrap();
    let srv = dir.path().join("srv");
    std::fs::create_dir_all(srv.join("state/.git")).unwrap();
    std::fs::write(srv.join("state/flake.nix"), "{ }").unwrap();
    std::fs::write(srv.join("state/.git/HEAD"), "ref").unwrap();
    for (path, mode) in [
        ("", 0o755),
        ("state", 0o755),
        ("state/.git", 0o755),
        ("state/flake.nix", 0o644),
        ("state/.git/HEAD", 0o444),
    ] {
        std::fs::set_permissions(srv.join(path), std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let files = Files::open_trusting(dir.path(), "r1", me().0).unwrap();
    let real = Real {
        dir,
        performer: Performer::new(files),
        runtime: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    };
    (Model(world), real)
}

fn same(model: &Model, real: &Real, after: &str) {
    let model = model.entries();
    let real = real.entries();
    let owners: BTreeMap<_, _> = model
        .iter()
        .map(|entry| (entry.0.clone(), entry.4))
        .collect();
    let comparable = |entries: Vec<Entry>| -> Vec<Entry> {
        entries
            .into_iter()
            .map(|(path, dir, mode, contents, owner)| {
                let owner = owners.get(&path).copied().flatten().and(owner);
                (path, dir, mode, contents, owner)
            })
            .collect()
    };
    assert_eq!(comparable(model), comparable(real), "after {after}");
}

const FORWARD: [Op; 12] = [
    Op::CreateDir("/srv/a", 0o750),
    Op::CreateDirs("/srv/d/e/f", 0o700),
    Op::CreateDirs("/srv/d/e/f", 0o700),
    Op::Put("/srv/a/f", "one", 0o640),
    Op::Put("/srv/a/f", "two", 0o640),
    Op::SetMode("/srv/a/f", 0o600),
    Op::SetAside("/srv/a/f"),
    Op::CreateDir("/srv/c", 0o700),
    Op::RemoveCreated("/srv/c"),
    Op::Copy("/srv/state", "/srv/b", 0o700),
    Op::RemoveCreatedTree("/srv/b"),
    Op::Reclaim("/srv/state", 0o700),
];

#[test]
fn every_file_action_and_its_undo_leave_what_the_model_predicts() {
    let (mut model, mut real) = sides();
    same(&model, &real, "setup");
    let mut undos = (Vec::new(), Vec::new());

    for op in FORWARD {
        let (on_model, on_real) = (action(op, &mut model), action(op, &mut real));
        undos.0.push(model.perform(&on_model));
        undos.1.push(real.perform(&on_real));
        same(&model, &real, &format!("{on_model:?}"));
    }
    for (undo_model, undo_real) in rollback_order(&undos.0)
        .into_iter()
        .zip(rollback_order(&undos.1))
    {
        model.perform(&undo_model);
        real.perform(&undo_real);
        same(&model, &real, &format!("undoing with {undo_model:?}"));
    }
}

#[test]
fn a_commit_leaves_what_the_model_predicts() {
    let (mut model, mut real) = sides();

    for op in FORWARD.into_iter().chain([Op::Commit]) {
        let (on_model, on_real) = (action(op, &mut model), action(op, &mut real));
        model.perform(&on_model);
        real.perform(&on_real);
        same(&model, &real, &format!("{on_model:?}"));
    }
    assert!(
        !model
            .entries()
            .iter()
            .any(|entry| entry.0.contains("<kept by mix>")),
        "{:?}",
        model.entries()
    );
}
