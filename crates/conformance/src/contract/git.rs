use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::{
    FLAKE_NIX, GIT_DIR, HOME_NIX, INDEX_LOCK, REPOSITORY_CONFIG, mix_state_dir, repository_dir,
};
use mix_core::effect::{Action, Fact, Query};
use mix_core::model::{Content, World};
use mix_shell::drive::Performer;
use mix_shell::effect::files::Files;
use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};

use super::{class, observe, perform, runtime};

pub static GIT: OnceLock<PathBuf> = OnceLock::new();

const HOME: &str = "/var/tmp/mix-contract-git";
const FILES: [&str; 2] = [FLAKE_NIX, HOME_NIX];
const CONTENTS: [&[u8]; 3] = [b"one\n", b"two\n", b"three\n"];
const STRAY: &str = "stray";
const ROOT: (u32, u32) = (0, 0);
const HOSTILE_CONFIG: &str = "[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = /bin/false\n";
const HOOKS: [&str; 3] = ["pre-commit", "post-commit", "reference-transaction"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Content(usize),
    Commit,
    Head,
    Index,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Missing,
    Directory,
    Junk,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Write { file: &'static str, contents: usize },
    Remove(&'static str),
    Stray,
    Record,
    Create,
    Discard,
    Lock,
    Unlock,
    Hostile,
    Outer,
    Damage(Target, Kind),
}

pub fn known() -> Vec<Vec<Op>> {
    vec![vec![
        Op::Write {
            file: FLAKE_NIX,
            contents: 0,
        },
        Op::Record,
        Op::Write {
            file: FLAKE_NIX,
            contents: 1,
        },
        Op::Record,
        Op::Damage(Target::Commit, Kind::Missing),
        Op::Record,
    ]]
}

fn user() -> InvokingUser {
    InvokingUser {
        uid: ROOT.0,
        gid: ROOT.1,
        name: "root".to_string(),
        home: PathBuf::from(HOME),
    }
}

fn state_dir() -> PathBuf {
    mix_state_dir(Path::new(HOME))
}

fn repository() -> PathBuf {
    repository_dir(Path::new(HOME))
}

fn fixed(target: Target) -> Option<PathBuf> {
    match target {
        Target::Head => Some(repository().join("HEAD")),
        Target::Index => Some(repository().join("index")),
        Target::Config => Some(repository().join("config")),
        Target::Content(_) | Target::Commit => None,
    }
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub world: World,
    pub commits: Vec<PathBuf>,
    pub outcome: Option<String>,
}

impl Reference {
    fn object(&self, target: Target) -> Option<PathBuf> {
        match target {
            Target::Content(contents) => Some(World::blob_object(&user(), CONTENTS[contents])),
            Target::Commit => self.world.branch_object(&user()),
            fixed_target => fixed(fixed_target),
        }
    }

    fn other(&self, target: Target) -> Option<PathBuf> {
        match target {
            Target::Content(contents) => (0..CONTENTS.len())
                .filter(|other| *other != contents)
                .map(|other| World::blob_object(&user(), CONTENTS[other]))
                .find(|path| self.is_file(path)),
            Target::Commit => self.commits.last().cloned(),
            _ => None,
        }
    }

    fn is_file(&self, path: &Path) -> bool {
        matches!(
            self.world.files.get(path).map(|entry| &entry.content),
            Some(Content::File(_))
        )
    }

    fn remove(&mut self, path: &Path) {
        self.world.files.retain(|found, _| !found.starts_with(path));
    }

    fn damage(&mut self, target: Target, kind: Kind) -> &'static str {
        let Some(path) = self.object(target) else {
            return "untouched";
        };
        let Some(entry) = self.world.files.get(&path).cloned() else {
            return "untouched";
        };
        match kind {
            Kind::Missing => self.remove(&path),
            Kind::Directory => {
                self.remove(&path);
                self.world.with_dir(&path, 0o755, ROOT);
            }
            Kind::Junk => {
                self.remove(&path);
                self.world
                    .with_file(&path, b"junk", entry.mode, entry.owner);
            }
            Kind::Other => {
                let Some(source) = self.other(target).filter(|source| self.is_file(source)) else {
                    return "untouched";
                };
                let Some(Content::File(bytes)) = self
                    .world
                    .files
                    .get(&source)
                    .map(|entry| entry.content.clone())
                else {
                    return "untouched";
                };
                self.remove(&path);
                self.world.with_file(&path, &bytes, entry.mode, entry.owner);
            }
        }
        "damaged"
    }

    fn record(&mut self, action: &Action) -> String {
        let before = self.world.branch_object(&user());
        let outcome = class(&self.world.apply(action));
        let after = self.world.branch_object(&user());
        moved(&mut self.commits, before, after, outcome)
    }
}

fn moved(
    commits: &mut Vec<PathBuf>,
    before: Option<PathBuf>,
    after: Option<PathBuf>,
    outcome: String,
) -> String {
    if before == after {
        return format!("{outcome}, head stayed");
    }
    commits.extend(before);
    format!("{outcome}, head moved")
}

pub struct Git;

impl ReferenceStateMachine for Git {
    type State = Reference;
    type Transition = Op;

    fn init_state() -> BoxedStrategy<Reference> {
        let mut world = World::default();
        let state = state_dir();
        for dir in state.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if !world.files.contains_key(dir) {
                world.with_dir(dir, 0o755, ROOT);
            }
        }
        Just(Reference {
            world,
            commits: Vec::new(),
            outcome: None,
        })
        .boxed()
    }

    fn transitions(_: &Reference) -> BoxedStrategy<Op> {
        let file = select(&FILES[..]);
        let objects = prop_oneof![
            (0..CONTENTS.len()).prop_map(Target::Content),
            Just(Target::Commit),
        ];
        let objects_damage = (
            objects,
            select(&[Kind::Missing, Kind::Directory, Kind::Junk, Kind::Other][..]),
        )
            .prop_map(|(target, kind)| Op::Damage(target, kind));
        let files_damage = (
            select(&[Target::Head, Target::Index, Target::Config][..]),
            select(&[Kind::Missing, Kind::Directory, Kind::Junk][..]),
        )
            .prop_map(|(target, kind)| Op::Damage(target, kind));
        prop_oneof![
            4 => (file.clone(), 0..CONTENTS.len())
                .prop_map(|(file, contents)| Op::Write { file, contents }),
            1 => file.prop_map(Op::Remove),
            1 => Just(Op::Stray),
            4 => Just(Op::Record),
            2 => Just(Op::Create),
            1 => Just(Op::Discard),
            1 => Just(Op::Lock),
            1 => Just(Op::Unlock),
            1 => Just(Op::Hostile),
            1 => Just(Op::Outer),
            3 => objects_damage,
            2 => files_damage,
        ]
        .boxed()
    }

    fn apply(mut state: Reference, op: &Op) -> Reference {
        let outcome = match op {
            Op::Write { file, contents } => {
                let path = state_dir().join(file);
                state.remove(&path);
                state
                    .world
                    .with_file(&path, CONTENTS[*contents], 0o644, ROOT);
                "written".to_string()
            }
            Op::Remove(file) => {
                let path = state_dir().join(file);
                let found = state.world.files.contains_key(&path);
                state.remove(&path);
                if found { "removed" } else { "absent" }.to_string()
            }
            Op::Stray => {
                let path = state_dir().join(STRAY);
                state.remove(&path);
                state.world.with_file(&path, b"stray\n", 0o644, ROOT);
                "written".to_string()
            }
            Op::Record => state.record(&Action::RecordState { user: user() }),
            Op::Create => state.record(&Action::CreateRepository { user: user() }),
            Op::Discard => {
                let found = state.world.files.contains_key(&repository());
                state.remove(&repository());
                if found { "discarded" } else { "absent" }.to_string()
            }
            Op::Lock => {
                if state.world.files.contains_key(&repository()) && !state.is_file(&repository()) {
                    let lock = repository().join(INDEX_LOCK);
                    state.remove(&lock);
                    state.world.with_file(&lock, b"", 0o644, ROOT);
                    "locked".to_string()
                } else {
                    "untouched".to_string()
                }
            }
            Op::Unlock => {
                let lock = repository().join(INDEX_LOCK);
                let found = state.world.files.contains_key(&lock);
                state.remove(&lock);
                if found { "unlocked" } else { "absent" }.to_string()
            }
            Op::Hostile => {
                let config = repository().join(REPOSITORY_CONFIG);
                if state.world.files.contains_key(&repository())
                    && !state.is_file(&repository())
                    && let Some(entry) = state.world.files.get(&config).cloned()
                    && let Content::File(bytes) = &entry.content
                {
                    let mut hostile = bytes.to_vec();
                    hostile.push(b'\n');
                    hostile.extend_from_slice(HOSTILE_CONFIG.as_bytes());
                    state.remove(&config);
                    state
                        .world
                        .with_file(&config, &hostile, entry.mode, entry.owner);
                }
                "hostile".to_string()
            }
            Op::Outer => "outer".to_string(),
            Op::Damage(target, kind) => state.damage(*target, *kind).to_string(),
        };
        state.outcome = Some(outcome);
        state
    }
}

pub struct Real {
    runtime: tokio::runtime::Runtime,
    performer: Performer,
    commits: Vec<PathBuf>,
}

fn git() -> &'static Path {
    GIT.get().expect("the git suite is given --git")
}

fn command(args: &[&str]) -> mix_exec::Command {
    mix_exec::Command::new(git())
        .args(args)
        .env_clear()
        .env("HOME", HOME)
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
}

fn stdout(args: &[&str]) -> Option<String> {
    let output = command(args)
        .output_blocking(&mix_exec::Scope::root())
        .expect("the pinned git runs");
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn inspect() -> String {
    let repository = repository();
    let repository = repository.to_string_lossy();
    [
        &["rev-parse", "HEAD"][..],
        &["cat-file", "-t", "HEAD"],
        &["fsck", "--connectivity-only", "--no-dangling"],
        &["ls-files", "--stage"],
        &["config", "--list", "--local"],
    ]
    .iter()
    .map(|args| {
        let output = command(&[&["--git-dir", &repository][..], args].concat())
            .output_blocking(&mix_exec::Scope::root())
            .expect("the pinned git runs");
        format!(
            "git {}: {:?} {}{}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn loose(id: &str) -> PathBuf {
    repository().join("objects").join(&id[..2]).join(&id[2..])
}

fn sample(contents: usize) -> PathBuf {
    Path::new(HOME).join("samples").join(contents.to_string())
}

fn marker() -> PathBuf {
    Path::new(HOME).join("hook-ran")
}

#[expect(
    clippy::disallowed_methods,
    reason = "the contract owns its scratch home"
)]
fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("the scratch home is ours");
    std::fs::write(path, bytes).expect("the scratch home is ours");
}

#[expect(
    clippy::disallowed_methods,
    reason = "the contract owns its scratch home"
)]
fn hook(path: &Path) {
    write(
        path,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker().display()).as_bytes(),
    );
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("the scratch home is ours");
}

#[expect(
    clippy::disallowed_methods,
    reason = "the contract owns its scratch home"
)]
fn remove(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path).is_ok(),
        Ok(_) => std::fs::remove_file(path).is_ok(),
        Err(_) => false,
    }
}

impl Real {
    fn object(&self, target: Target) -> Option<PathBuf> {
        match target {
            Target::Content(contents) => {
                let sample = sample(contents);
                stdout(&["hash-object", "--", &sample.to_string_lossy()]).map(|id| loose(&id))
            }
            Target::Commit => std::fs::read_to_string(repository().join("refs/heads/main"))
                .ok()
                .map(|id| loose(id.trim())),
            fixed_target => fixed(fixed_target),
        }
    }

    fn other(&self, target: Target) -> Option<PathBuf> {
        match target {
            Target::Content(contents) => (0..CONTENTS.len())
                .filter(|other| *other != contents)
                .filter_map(|other| self.object(Target::Content(other)))
                .find(|path| path.is_file()),
            Target::Commit => self.commits.last().cloned(),
            _ => None,
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the contract damages its own scratch home on purpose"
    )]
    fn damage(&self, target: Target, kind: Kind) -> &'static str {
        let Some(path) = self.object(target) else {
            return "untouched";
        };
        if std::fs::symlink_metadata(&path).is_err() {
            return "untouched";
        }
        match kind {
            Kind::Missing => {
                remove(&path);
            }
            Kind::Directory => {
                remove(&path);
                std::fs::create_dir(&path).expect("the scratch home is ours");
            }
            Kind::Junk => {
                remove(&path);
                write(&path, b"junk");
            }
            Kind::Other => {
                let Some(bytes) = self
                    .other(target)
                    .filter(|source| source.is_file())
                    .and_then(|source| std::fs::read(source).ok())
                else {
                    return "untouched";
                };
                remove(&path);
                write(&path, &bytes);
            }
        }
        "damaged"
    }

    fn branch(&self) -> Option<PathBuf> {
        std::fs::read_to_string(repository().join("refs/heads/main"))
            .ok()
            .map(|id| loose(id.trim()))
    }

    fn record(&mut self, action: &Action) -> String {
        let before = self.branch();
        let outcome = class(&perform(&self.runtime, &mut self.performer, action));
        let after = self.branch();
        moved(&mut self.commits, before, after, outcome)
    }

    fn hostile(&self) {
        let user_config = format!("[core]\n\thooksPath = {HOME}/hooks\n{HOSTILE_CONFIG}");
        write(&Path::new(HOME).join(".gitconfig"), user_config.as_bytes());
        write(
            &Path::new(HOME).join(".config/git/config"),
            user_config.as_bytes(),
        );
        for name in HOOKS {
            hook(&Path::new(HOME).join("hooks").join(name));
        }
        if repository().is_dir() {
            let config = repository().join("config");
            if let Ok(mut found) = std::fs::read_to_string(&config) {
                found.push('\n');
                found.push_str(HOSTILE_CONFIG);
                write(&config, found.as_bytes());
            }
            for name in HOOKS {
                hook(&repository().join("hooks").join(name));
            }
        }
    }

    fn outer(&self) {
        let home = Path::new(HOME);
        if home.join(GIT_DIR).exists() {
            return;
        }
        let home = home.to_string_lossy();
        stdout(&["init", "-q", "--initial-branch=main", "--template=", &home])
            .expect("an outer repository");
        write(&Path::new(HOME).join("outer"), b"outer\n");
        stdout(&["-C", &home, "add", "outer"]).expect("staged");
        stdout(&[
            "-C",
            &home,
            "-c",
            "user.name=contract",
            "-c",
            "user.email=contract@localhost",
            "commit",
            "-q",
            "-m",
            "outer",
        ])
        .expect("committed");
    }

    fn tracked(&mut self) -> Option<Vec<String>> {
        let repository = repository();
        let heads = repository.join("refs/heads");
        let repository = repository.to_string_lossy();
        let branch = ["rev-parse", "--verify", "--quiet", "refs/heads/main"];
        if (heads.is_dir() || !heads.exists())
            && stdout(&["--git-dir", &repository, "symbolic-ref", "--quiet", "HEAD"]).as_deref()
                == Some("refs/heads/main")
            && stdout(&[&["--git-dir", &repository][..], &branch].concat()).is_none()
        {
            return Some(Vec::new());
        }
        let verified = ["rev-parse", "--verify", "--quiet", "HEAD^{tree}"];
        stdout(&[&["--git-dir", &repository][..], &verified].concat())?;
        let listed = stdout(&["--git-dir", &repository, "ls-tree", "--name-only", "HEAD"])?;
        Some(listed.lines().map(str::to_string).collect())
    }
}

fn model_tracked(world: &World) -> Option<Vec<String>> {
    if !world.verifies(&user()) {
        return None;
    }
    world
        .committed(&user())
        .map(|committed| committed.keys().cloned().collect())
}

fn same(state: &Reference, real: &mut Real, after: &str) {
    let config = Query::Contents(repository().join(REPOSITORY_CONFIG));
    assert_eq!(
        state.world.observe(&config),
        observe(&real.runtime, &mut real.performer, &config),
        "the repository config after {after}"
    );
    let query = Query::Repository(user());
    let expected = state.world.observe(&query);
    let found = observe(&real.runtime, &mut real.performer, &query);
    assert_eq!(
        expected,
        found,
        "{query:?} after {after}; the machine said {}; git:\n{}",
        super::failed(),
        inspect()
    );
    if matches!(found, Fact::Repository { intact: true, .. }) {
        assert_eq!(
            model_tracked(&state.world),
            real.tracked(),
            "the committed files after {after}"
        );
    }
    assert!(!marker().exists(), "a hook ran after {after}");
}

fn clear() {
    remove(Path::new(HOME));
}

impl StateMachineTest for Git {
    type SystemUnderTest = Real;
    type Reference = Git;

    #[expect(
        clippy::disallowed_methods,
        reason = "the contract owns its scratch home"
    )]
    fn init_test(state: &Reference) -> Real {
        clear();
        std::fs::create_dir_all(state_dir()).expect("the scratch home is ours");
        let profile = Path::new(HOME).join(".nix-profile/bin");
        std::fs::create_dir_all(&profile).expect("the scratch home is ours");
        std::os::unix::fs::symlink(git(), profile.join("git")).expect("the profile links git");
        for (index, contents) in CONTENTS.iter().enumerate() {
            write(&sample(index), contents);
        }
        let mut real = Real {
            runtime: runtime(),
            performer: Performer::new(
                Files::open(Path::new("/"), super::request()).expect("the root opens"),
            ),
            commits: Vec::new(),
        };
        same(state, &mut real, "starting");
        real
    }

    fn apply(mut real: Real, state: &Reference, op: Op) -> Real {
        let outcome = match &op {
            Op::Write { file, contents } => {
                let path = state_dir().join(file);
                remove(&path);
                write(&path, CONTENTS[*contents]);
                "written".to_string()
            }
            Op::Remove(file) => if remove(&state_dir().join(file)) {
                "removed"
            } else {
                "absent"
            }
            .to_string(),
            Op::Stray => {
                write(&state_dir().join(STRAY), b"stray\n");
                "written".to_string()
            }
            Op::Record => real.record(&Action::RecordState { user: user() }),
            Op::Create => real.record(&Action::CreateRepository { user: user() }),
            Op::Discard => if remove(&repository()) {
                "discarded"
            } else {
                "absent"
            }
            .to_string(),
            Op::Lock => {
                if repository().is_dir() {
                    write(&repository().join(INDEX_LOCK), b"");
                    "locked".to_string()
                } else {
                    "untouched".to_string()
                }
            }
            Op::Unlock => if remove(&repository().join(INDEX_LOCK)) {
                "unlocked"
            } else {
                "absent"
            }
            .to_string(),
            Op::Hostile => {
                real.hostile();
                "hostile".to_string()
            }
            Op::Outer => {
                real.outer();
                "outer".to_string()
            }
            Op::Damage(target, kind) => real.damage(*target, *kind).to_string(),
        };
        assert_eq!(
            state.outcome.as_deref().unwrap_or_default(),
            outcome,
            "{op:?}: the model and git disagree; git:\n{}",
            inspect()
        );
        same(state, &mut real, &format!("{op:?}"));
        real
    }

    fn teardown(_: Real, _: Reference) {
        clear();
    }
}
