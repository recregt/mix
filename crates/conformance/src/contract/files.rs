use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_core::action::{Action, Expect, Fact, FileId, Owner, PathFacts, Query, rollback_order};
use mix_core::paths::is_leftover;
use mix_core::testkit::{Breakage, Damage};
use mix_core::world::World;
use mix_shell::drive::Performer;
use mix_shell::effect::files::Files;
use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};

use super::{class, comparable, observe, perform, runtime};

pub const REQUEST: &str = "contract";

const NAMES: [&str; 3] = ["a", "b", "c"];
const MODES: [u32; 5] = [0o755, 0o700, 0o644, 0o600, 0o000];
const OWNERS: [Owner; 3] = [(0, 0), (1000, 1000), (4242, 4242)];
const CONTENTS: [&[u8]; 3] = [b"", b"one", b"two\n"];
const STRANGER: Owner = (4242, 4242);
const UNKNOWN: FileId = FileId {
    dev: 0,
    ino: u64::MAX,
    born: None,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guess {
    Current,
    Absent,
    Wrong,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    CreateDir {
        path: PathBuf,
        mode: u32,
        owner: Option<Owner>,
    },
    CreateDirs {
        path: PathBuf,
        mode: u32,
        owner: Option<Owner>,
    },
    PutFile {
        path: PathBuf,
        contents: usize,
        mode: u32,
        owner: Option<Owner>,
        expect: Guess,
    },
    SetMode {
        path: PathBuf,
        mode: u32,
        expect: Guess,
    },
    SetOwner {
        path: PathBuf,
        owner: Owner,
        expect: Guess,
    },
    SetAside {
        path: PathBuf,
        expect: Guess,
    },
    RemoveCreated {
        path: PathBuf,
        expect: Guess,
    },
    RemoveCreatedTree {
        path: PathBuf,
        expect: Guess,
    },
    Restore {
        path: PathBuf,
        from: usize,
        expect: Guess,
    },
    ReclaimTree {
        path: PathBuf,
        expect: Guess,
        owner: Owner,
        mode: u32,
    },
    CopyTree {
        from: PathBuf,
        to: PathBuf,
        owner: Owner,
        mode: u32,
    },
    Damage(Breakage),
}

fn expect(guess: Guess, facts: &PathFacts) -> Expect {
    match (guess, facts.id) {
        (Guess::Current, Some(id)) => Expect::Present(id),
        (Guess::Current, None) | (Guess::Absent, _) => Expect::Absent,
        (Guess::Wrong, _) => Expect::Present(UNKNOWN),
    }
}

fn id(guess: Guess, facts: &PathFacts) -> FileId {
    match (guess, facts.id) {
        (Guess::Current, Some(id)) => id,
        _ => UNKNOWN,
    }
}

fn mode(guess: Guess, facts: &PathFacts) -> u32 {
    match guess {
        Guess::Current => facts.mode,
        Guess::Absent | Guess::Wrong => facts.mode ^ 0o100,
    }
}

fn owner(guess: Guess, facts: &PathFacts) -> Owner {
    match guess {
        Guess::Current => facts.owner,
        Guess::Absent | Guess::Wrong => (facts.owner.0 ^ 1, facts.owner.1 ^ 1),
    }
}

fn concrete(op: &Op, facts: &mut dyn FnMut(&Path) -> PathFacts, siblings: &[PathBuf]) -> Action {
    match op {
        Op::CreateDir { path, mode, owner } => Action::CreateDir {
            path: path.clone(),
            mode: *mode,
            owner: *owner,
        },
        Op::CreateDirs { path, mode, owner } => Action::CreateDirs {
            path: path.clone(),
            mode: *mode,
            owner: *owner,
        },
        Op::PutFile {
            path,
            contents,
            mode,
            owner,
            expect: guess,
        } => Action::PutFile {
            path: path.clone(),
            contents: Arc::from(CONTENTS[*contents]),
            mode: *mode,
            owner: *owner,
            expect: expect(*guess, &facts(path)),
        },
        Op::SetMode {
            path,
            mode: wanted,
            expect: guess,
        } => Action::SetMode {
            path: path.clone(),
            mode: *wanted,
            expect: mode(*guess, &facts(path)),
        },
        Op::SetOwner {
            path,
            owner: wanted,
            expect: guess,
        } => Action::SetOwner {
            path: path.clone(),
            owner: *wanted,
            expect: owner(*guess, &facts(path)),
        },
        Op::SetAside {
            path,
            expect: guess,
        } => Action::SetAside {
            path: path.clone(),
            expect: id(*guess, &facts(path)),
        },
        Op::RemoveCreated {
            path,
            expect: guess,
        } => Action::RemoveCreated {
            path: path.clone(),
            expect: id(*guess, &facts(path)),
        },
        Op::RemoveCreatedTree {
            path,
            expect: guess,
        } => Action::RemoveCreatedTree {
            path: path.clone(),
            expect: id(*guess, &facts(path)),
        },
        Op::Restore {
            path,
            from,
            expect: guess,
        } => Action::Restore {
            path: path.clone(),
            from: if siblings.is_empty() {
                path.with_file_name(format!(".none.mix-backup-{REQUEST}-0"))
            } else {
                siblings[from % siblings.len()].clone()
            },
            expect: expect(*guess, &facts(path)),
        },
        Op::ReclaimTree {
            path,
            expect: guess,
            owner,
            mode,
        } => Action::ReclaimTree {
            path: path.clone(),
            expect: id(*guess, &facts(path)),
            owner: *owner,
            mode: *mode,
        },
        Op::CopyTree {
            from,
            to,
            owner,
            mode,
        } => Action::CopyTree {
            from: from.clone(),
            to: to.clone(),
            owner: *owner,
            mode: *mode,
        },
        Op::Damage(_) => unreachable!("damage is not an action"),
    }
}

fn counter(name: &str) -> Option<(&str, u64)> {
    let (prefix, n) = name.rsplit_once('-')?;
    Some((prefix, n.parse().ok()?))
}

fn normalized(paths: &[PathBuf]) -> BTreeMap<PathBuf, PathBuf> {
    let mut groups: BTreeMap<(PathBuf, String), Vec<(u64, String)>> = BTreeMap::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_leftover(name) {
            continue;
        }
        if let Some((prefix, n)) = counter(name) {
            groups
                .entry((
                    path.parent().unwrap_or(Path::new("/")).to_path_buf(),
                    prefix.to_string(),
                ))
                .or_default()
                .push((n, name.to_string()));
        }
    }
    let mut renamed: BTreeMap<(PathBuf, String), String> = BTreeMap::new();
    for ((parent, prefix), mut names) in groups {
        names.sort();
        for (rank, (_, name)) in names.into_iter().enumerate() {
            renamed.insert((parent.clone(), name), format!("{prefix}-#{rank}"));
        }
    }
    paths
        .iter()
        .map(|path| {
            let mut concrete = PathBuf::from("/");
            let mut shown = PathBuf::from("/");
            for component in path.components().skip(1) {
                let name = component.as_os_str().to_string_lossy().into_owned();
                let named = renamed
                    .get(&(concrete.clone(), name.clone()))
                    .cloned()
                    .unwrap_or_else(|| name.clone());
                concrete.push(&name);
                shown.push(named);
            }
            (shown, path.clone())
        })
        .collect()
}

fn siblings(paths: &[PathBuf]) -> Vec<PathBuf> {
    normalized(paths)
        .into_iter()
        .filter(|(_, path)| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_leftover)
        })
        .map(|(_, path)| path)
        .collect()
}

type Seen = BTreeMap<PathBuf, (Fact, Fact, Fact)>;

fn seen(paths: &[PathBuf], mut ask: impl FnMut(&Query) -> Fact) -> Seen {
    normalized(paths)
        .into_iter()
        .filter(|(_, path)| path != Path::new("/"))
        .map(|(shown, path)| {
            (
                shown,
                (
                    comparable(ask(&Query::Path(path.clone()))),
                    ask(&Query::Contents(path.clone())),
                    ask(&Query::TreeOwner(path)),
                ),
            )
        })
        .collect()
}

pub fn bare() -> World {
    let mut world = World::default();
    world.files.retain(|path, _| path == Path::new("/"));
    world.acting_for = REQUEST.to_string();
    world
}

fn model_paths(world: &World) -> Vec<PathBuf> {
    world.files.keys().cloned().collect()
}

fn model_seen(world: &World) -> Seen {
    seen(&model_paths(world), |query| world.observe(query))
}

fn path_facts(fact: Fact) -> PathFacts {
    match fact {
        Fact::Path(facts) => facts,
        other => panic!("a path query answered {other:?}"),
    }
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub world: World,
    pub undo: Vec<Vec<Action>>,
    pub outcome: Option<String>,
    pub broken: bool,
}

pub struct FileTree;

fn place() -> impl Strategy<Value = PathBuf> {
    proptest::collection::vec(select(&NAMES[..]), 1..=3).prop_map(|names| {
        let mut path = PathBuf::from("/");
        path.extend(names);
        path
    })
}

impl ReferenceStateMachine for FileTree {
    type State = Reference;
    type Transition = Op;

    fn init_state() -> BoxedStrategy<Reference> {
        Just(Reference {
            world: bare(),
            undo: Vec::new(),
            outcome: None,
            broken: false,
        })
        .boxed()
    }

    fn transitions(state: &Reference) -> BoxedStrategy<Op> {
        let existing: Vec<PathBuf> = state
            .world
            .files
            .keys()
            .filter(|path| *path != Path::new("/"))
            .cloned()
            .collect();
        let path = if existing.is_empty() {
            place().boxed()
        } else {
            prop_oneof![place(), select(existing.clone())].boxed()
        };
        let guess = prop_oneof![
            3 => Just(Guess::Current),
            1 => Just(Guess::Absent),
            1 => Just(Guess::Wrong),
        ];
        let mode = select(&MODES[..]);
        let owner = select(&OWNERS[..]);
        let maybe = proptest::option::of(select(&OWNERS[..]));
        let ops = prop_oneof![
            (path.clone(), mode.clone(), maybe.clone())
                .prop_map(|(path, mode, owner)| Op::CreateDir { path, mode, owner }),
            (path.clone(), mode.clone(), maybe.clone())
                .prop_map(|(path, mode, owner)| Op::CreateDirs { path, mode, owner }),
            (
                path.clone(),
                0..CONTENTS.len(),
                mode.clone(),
                maybe,
                guess.clone()
            )
                .prop_map(|(path, contents, mode, owner, expect)| Op::PutFile {
                    path,
                    contents,
                    mode,
                    owner,
                    expect,
                }),
            (path.clone(), mode.clone(), guess.clone())
                .prop_map(|(path, mode, expect)| Op::SetMode { path, mode, expect }),
            (path.clone(), owner.clone(), guess.clone()).prop_map(|(path, owner, expect)| {
                Op::SetOwner {
                    path,
                    owner,
                    expect,
                }
            }),
            (path.clone(), guess.clone()).prop_map(|(path, expect)| Op::SetAside { path, expect }),
            (path.clone(), guess.clone())
                .prop_map(|(path, expect)| Op::RemoveCreated { path, expect }),
            (path.clone(), guess.clone())
                .prop_map(|(path, expect)| Op::RemoveCreatedTree { path, expect }),
            (path.clone(), any::<usize>(), guess.clone())
                .prop_map(|(path, from, expect)| Op::Restore { path, from, expect }),
            (path.clone(), guess, owner.clone(), mode.clone()).prop_map(
                |(path, expect, owner, mode)| Op::ReclaimTree {
                    path,
                    expect,
                    owner,
                    mode,
                }
            ),
            (path.clone(), path, owner, mode).prop_map(|(from, to, owner, mode)| Op::CopyTree {
                from,
                to,
                owner,
                mode,
            }),
        ];
        if existing.is_empty() {
            ops.boxed()
        } else {
            let damage = (select(existing), select(&Damage::ALL[..]))
                .prop_map(|(path, damage)| Op::Damage(Breakage { path, damage }));
            prop_oneof![5 => ops, 1 => damage].boxed()
        }
    }

    fn apply(mut state: Reference, op: &Op) -> Reference {
        if let Op::Damage(breakage) = op {
            let damaged = breakage.apply(&mut state.world);
            state.broken |= damaged;
            state.outcome = Some(format!("damaged {damaged}"));
            return state;
        }
        let paths = model_paths(&state.world);
        let world = state.world.clone();
        let action = concrete(
            op,
            &mut |path| path_facts(world.observe(&Query::Path(path.to_path_buf()))),
            &siblings(&paths),
        );
        let outcome = state.world.apply(&action);
        if let Ok(performed) = &outcome {
            state.undo.push(performed.undo.clone());
        }
        state.outcome = Some(class(&outcome));
        state
    }
}

pub struct Real {
    runtime: tokio::runtime::Runtime,
    dir: tempfile::TempDir,
    performer: Performer,
    undo: Vec<Vec<Action>>,
}

impl Real {
    fn on_disk(&self, path: &Path) -> PathBuf {
        self.dir
            .path()
            .join(path.strip_prefix("/").expect("model paths are absolute"))
    }

    fn paths(&self) -> Vec<PathBuf> {
        let mut found = vec![PathBuf::from("/")];
        let mut pending = vec![PathBuf::from("/")];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(self.on_disk(&dir)) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = dir.join(entry.file_name());
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    pending.push(path.clone());
                }
                found.push(path);
            }
        }
        found.sort();
        found
    }

    fn ask(&mut self, query: &Query) -> Fact {
        observe(&self.runtime, &mut self.performer, query)
    }

    fn seen(&mut self) -> Seen {
        let paths = self.paths();
        seen(&paths, |query| self.ask(query))
    }

    fn act(&mut self, op: &Op) -> String {
        let siblings = siblings(&self.paths());
        let action = {
            let runtime = &self.runtime;
            let performer = &mut self.performer;
            concrete(
                op,
                &mut |path| {
                    path_facts(observe(
                        runtime,
                        performer,
                        &Query::Path(path.to_path_buf()),
                    ))
                },
                &siblings,
            )
        };
        let outcome = perform(&self.runtime, &mut self.performer, &action);
        if let Ok(performed) = &outcome {
            self.undo.push(performed.undo.clone());
        }
        class(&outcome)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the contract damages its own scratch tree on purpose"
    )]
    fn damage(&self, breakage: &Breakage) -> bool {
        let path = self.on_disk(&breakage.path);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        let mode = meta.mode() & 0o7777;
        let owner = (meta.uid(), meta.gid());
        let dir = meta.is_dir();
        let chmod = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .expect("the contract owns its scratch tree");
        };
        let chown = |path: &Path, owner: Owner| {
            std::os::unix::fs::lchown(path, Some(owner.0), Some(owner.1))
                .expect("the contract runs as root");
        };
        match breakage.damage {
            Damage::Removed if dir => std::fs::remove_dir_all(&path).expect("removable"),
            Damage::Removed => std::fs::remove_file(&path).expect("removable"),
            Damage::Swapped if dir => {
                std::fs::remove_dir_all(&path).expect("removable");
                std::fs::write(&path, b"swapped").expect("writable");
                chmod(&path, mode & 0o666);
                chown(&path, owner);
            }
            Damage::Swapped => {
                std::fs::remove_file(&path).expect("removable");
                std::fs::create_dir(&path).expect("creatable");
                chmod(&path, mode | 0o111);
                chown(&path, owner);
            }
            Damage::Emptied if !dir && meta.len() > 0 => {
                std::fs::write(&path, b"").expect("writable");
            }
            Damage::Altered if !dir => {
                let mut bytes = std::fs::read(&path).expect("readable");
                match bytes.last_mut() {
                    Some(last) => *last ^= 0x20,
                    None => bytes.push(b'x'),
                }
                std::fs::write(&path, bytes).expect("writable");
            }
            Damage::Unreadable if mode & 0o777 != 0 => chmod(&path, mode & !0o777),
            Damage::Stranger if owner != STRANGER => chown(&path, STRANGER),
            Damage::Locked if !dir => {
                let mut name = path.file_name().unwrap_or_default().to_os_string();
                name.push(".lock");
                let lock = path.with_file_name(name);
                if lock.exists() {
                    return false;
                }
                std::fs::write(&lock, b"").expect("writable");
                chmod(&lock, 0o644);
                chown(&lock, owner);
            }
            _ => return false,
        }
        true
    }
}

fn same(model: &World, real: &mut Real, after: &str) {
    let expected = model_seen(model);
    let found = real.seen();
    if expected != found {
        let shown: Vec<String> = expected
            .keys()
            .chain(found.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|path| expected.get(*path) != found.get(*path))
            .map(|path| {
                format!(
                    "{}:\n  model {:?}\n  real  {:?}",
                    path.display(),
                    expected.get(path),
                    found.get(path)
                )
            })
            .collect();
        panic!(
            "after {after} the model and the disk differ:\n{}",
            shown.join("\n")
        );
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the contract owns its scratch root"
)]
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a scratch root");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("the scratch root is ours");
    dir
}

impl StateMachineTest for FileTree {
    type SystemUnderTest = Real;
    type Reference = FileTree;

    fn init_test(_: &Reference) -> Real {
        let dir = scratch();
        let performer =
            Performer::new(Files::open(dir.path(), REQUEST).expect("the scratch root opens"));
        Real {
            runtime: runtime(),
            dir,
            performer,
            undo: Vec::new(),
        }
    }

    fn apply(mut real: Real, state: &Reference, op: Op) -> Real {
        let outcome = match &op {
            Op::Damage(breakage) => format!("damaged {}", real.damage(breakage)),
            op => real.act(op),
        };
        let predicted = state.outcome.as_deref().unwrap_or_default();
        assert_eq!(
            predicted, outcome,
            "{op:?}: the model and the disk disagree"
        );
        same(&state.world, &mut real, &format!("{op:?}"));
        real
    }

    fn teardown(mut real: Real, mut state: Reference) {
        let model = rollback_order(&state.undo);
        let disk = rollback_order(&real.undo);
        assert_eq!(model.len(), disk.len());
        let mut clean = !state.broken;
        for (on_model, on_disk) in model.iter().zip(&disk) {
            let predicted = class(&state.world.apply(on_model));
            let outcome = class(&perform(&real.runtime, &mut real.performer, on_disk));
            assert_eq!(
                predicted, outcome,
                "undoing with {on_disk:?}: the model and the disk disagree"
            );
            clean &= outcome == "done";
            same(
                &state.world,
                &mut real,
                &format!("undoing with {on_disk:?}"),
            );
        }
        if clean {
            assert_eq!(
                model_seen(&state.world),
                model_seen(&bare()),
                "undoing everything did not leave the root empty"
            );
        }
    }
}
