use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_core::NoopActivity;
use mix_core::action::{
    Action, Expect, Fact, FileId, Owner, PathFacts, ProfileFacts, Query, UserSpec, rollback_order,
};
use mix_core::model::{Content, Profile, World};
use mix_core::paths::{
    DEFAULT_PROFILE_NIX_ENV, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, POLICY_FILE, mix_state_dir,
};
use mix_core::policy::Policy;
use mix_core::privilege::InvokingUser;
use mix_exec::Scope;
use mix_shell::HostConfig;
use mix_shell::drive::Performer;
use mix_shell::effect::files::Files;
use mix_shell::effect::generations::ProfileContext;

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

const FORWARD: [Op; 10] = [
    Op::CreateDir("/srv/a", 0o750),
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

type Step<'a> = &'a dyn Fn(&mut Machine, bool) -> Action;

struct Machine {
    world: World,
    performer: Performer,
    runtime: tokio::runtime::Runtime,
}

impl Machine {
    fn new(world: World) -> Self {
        Self::with(
            world,
            Performer::new(Files::open(Path::new("/"), "differential").unwrap()),
        )
    }

    fn with(world: World, performer: Performer) -> Self {
        Self {
            world,
            performer,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        }
    }

    fn real(&mut self, query: &Query) -> Fact {
        self.runtime
            .block_on(self.performer.observe(std::slice::from_ref(query)))
            .unwrap()
            .remove(0)
    }

    fn both(&mut self, query: &Query) -> (Fact, Fact) {
        (self.world.observe(query), self.real(query))
    }

    fn perform(&mut self, model: &Action, real: &Action) -> (Vec<Action>, Vec<Action>) {
        let predicted = self
            .world
            .apply(model)
            .unwrap_or_else(|failure| panic!("model: {model:?}: {failure:?}"));
        let scope = Scope::root();
        let mut progress = |_| {};
        let mut prepared = |_: &[Action]| Ok(());
        let performed = self
            .runtime
            .block_on(
                self.performer
                    .perform(real, &scope, &mut progress, &mut prepared),
            )
            .unwrap_or_else(|failure| panic!("real: {real:?}: {failure:?}"));
        (predicted.undo, performed.undo)
    }

    fn same(&mut self, queries: &[Query], after: &str) {
        for query in queries {
            let (model, real) = self.both(query);
            assert_eq!(
                comparable(model),
                comparable(real),
                "{query:?} after {after}"
            );
        }
    }

    fn run(&mut self, steps: &[Step<'_>], queries: &[Query]) {
        let baseline: Vec<Fact> = queries
            .iter()
            .map(|query| comparable(self.real(query)))
            .collect();
        let mut undos = (Vec::new(), Vec::new());
        for step in steps {
            let (model, real) = (step(self, true), step(self, false));
            let (on_model, on_real) = self.perform(&model, &real);
            undos.0.push(on_model);
            undos.1.push(on_real);
            self.same(queries, &format!("{real:?}"));
        }
        for (model, real) in rollback_order(&undos.0)
            .into_iter()
            .zip(rollback_order(&undos.1))
        {
            self.perform(&model, &real);
            self.same(queries, &format!("undoing with {real:?}"));
        }
        let after: Vec<Fact> = queries
            .iter()
            .map(|query| comparable(self.real(query)))
            .collect();
        assert_eq!(after, baseline, "the machine is back where it started");
    }
}

fn comparable(fact: Fact) -> Fact {
    match fact {
        Fact::Group(Some(mut group)) => {
            group.members.sort();
            Fact::Group(Some(group))
        }
        Fact::Unit(mut unit) => {
            unit.active_since = None;
            Fact::Unit(unit)
        }
        Fact::Path(facts) => Fact::Path(PathFacts {
            id: None,
            changed: None,
            digest: None,
            ..facts
        }),
        other => other,
    }
}

fn observed<T>(machine: &mut Machine, model: bool, query: Query, pick: fn(Fact) -> T) -> T {
    pick(if model {
        machine.world.observe(&query)
    } else {
        machine.real(&query)
    })
}

fn group_gid(fact: Fact) -> u32 {
    match fact {
        Fact::Group(Some(group)) => group.gid,
        other => panic!("{other:?}"),
    }
}

fn user_ids(fact: Fact) -> Owner {
    match fact {
        Fact::User(Some(user)) => (user.uid, user.gid),
        other => panic!("{other:?}"),
    }
}

fn user_comment(fact: Fact) -> String {
    match fact {
        Fact::User(Some(user)) => user.comment,
        other => panic!("{other:?}"),
    }
}

fn path_facts(fact: Fact) -> PathFacts {
    match fact {
        Fact::Path(facts) => facts,
        other => panic!("{other:?}"),
    }
}

#[test]
#[ignore = "needs root; e2e/test_differential.py runs it in a container"]
fn every_account_action_and_its_undo_match_the_model() {
    let mut machine = Machine::new(World::default());
    let group = || Query::Group("mixdiff".into());
    let extra = || Query::Group("mixdiff-extra".into());
    let user = || Query::User("mixdiff1".into());

    machine.run(
        &[
            &|_, _| Action::AddGroup {
                name: "mixdiff".into(),
                gid: 39_000,
            },
            &|_, _| Action::AddGroup {
                name: "mixdiff-extra".into(),
                gid: 39_002,
            },
            &|_, _| {
                Action::AddUser(UserSpec {
                    name: "mixdiff1".into(),
                    uid: 39_001,
                    gid: 39_000,
                    home: "/var/empty".into(),
                    shell: "/usr/sbin/nologin".into(),
                    comment: "mix differential".into(),
                    groups: vec!["mixdiff".into()],
                })
            },
            &|_, _| Action::AddMember {
                group: "mixdiff-extra".into(),
                user: "mixdiff1".into(),
            },
            &|machine, model| Action::SetUserIds {
                name: "mixdiff1".into(),
                ids: (39_011, 39_000),
                expect: observed(machine, model, user(), user_ids),
            },
            &|machine, model| Action::SetGroupGid {
                name: "mixdiff-extra".into(),
                gid: 39_012,
                expect: observed(machine, model, extra(), group_gid),
            },
            &|_, _| Action::RemoveMember {
                group: "mixdiff-extra".into(),
                user: "mixdiff1".into(),
            },
            &|machine, model| Action::DeleteUser {
                name: "mixdiff1".into(),
                expect: observed(machine, model, user(), user_ids),
                comment: observed(machine, model, user(), user_comment),
            },
        ],
        &[group(), extra(), user()],
    );
}

#[test]
#[ignore = "needs root and systemd; e2e/test_differential.py runs it in a container"]
fn every_unit_action_and_its_undo_match_the_model() {
    let mut machine = Machine::new(World::default());
    let unit = "mix-differential.service";
    let contents: Arc<[u8]> = Arc::from(
        &b"[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n\n[Install]\nWantedBy=multi-user.target\n"[..],
    );
    let install = {
        let contents = contents.clone();
        move |_: &mut Machine, _: bool| Action::InstallUnit {
            unit: unit.into(),
            contents: contents.clone(),
            expect: Expect::Absent,
        }
    };

    machine.run(
        &[
            &install,
            &|_, _| Action::DaemonReload,
            &|_, _| Action::EnableUnit { unit: unit.into() },
            &|_, _| Action::StartUnit { unit: unit.into() },
            &|_, _| Action::RestartUnit { unit: unit.into() },
            &|_, _| Action::StopUnit { unit: unit.into() },
            &|_, _| Action::DisableUnit { unit: unit.into() },
        ],
        &[Query::Unit(unit.into())],
    );
}

#[test]
#[ignore = "needs root; e2e/test_differential.py runs it in a container"]
fn changing_an_owner_to_another_user_matches_the_model() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("owned");
    let mut world = World::default();
    world.with_dir("/tmp", 0o1777, ROOT_OWNER);
    world.with_dir(dir.path(), 0o700, ROOT_OWNER);
    let mut machine = Machine::new(world);
    let at = path.clone();
    let put = move |_: &mut Machine, _: bool| Action::PutFile {
        path: at.clone(),
        contents: Arc::from(&b"x"[..]),
        mode: 0o640,
        owner: None,
        expect: Expect::Absent,
    };
    let at = path.clone();
    let chown = move |machine: &mut Machine, model: bool| Action::SetOwner {
        path: at.clone(),
        owner: (4_242, 4_243),
        expect: observed(machine, model, Query::Path(at.clone()), path_facts).owner,
    };

    machine.run(&[&put, &chown], &[Query::Path(path.clone())]);
}

fn enrolled() -> InvokingUser {
    let name = std::env::var("MIX_DIFFERENTIAL_USER").expect("the enrolled user's name");
    let user = nix::unistd::User::from_name(&name).unwrap().unwrap();
    InvokingUser {
        uid: user.uid.as_raw(),
        gid: user.gid.as_raw(),
        name,
        home: user.dir,
    }
}

fn profile_of(fact: Fact) -> ProfileFacts {
    match fact {
        Fact::Profile(profile) => profile,
        other => panic!("{other:?}"),
    }
}

#[test]
#[ignore = "needs a bootstrapped machine and its mirror; e2e/test_differential.py runs it"]
fn every_profile_action_and_its_undo_match_the_model() {
    let user = enrolled();
    let owner = (user.uid, user.gid);
    let state = mix_state_dir(&user.home);
    let config: Vec<Option<Arc<[u8]>>> = [FLAKE_NIX, HOME_NIX, FLAKE_LOCK]
        .iter()
        .map(|file| std::fs::read(state.join(file)).ok().map(Arc::from))
        .collect();
    let performer = Performer::new(Files::open(Path::new("/"), "differential").unwrap())
        .with_profile(ProfileContext {
            mirror: Policy::load(std::fs::read_to_string(POLICY_FILE).ok().as_deref())
                .mirror()
                .map(|mirror| mirror.url().to_string()),
            activity: Arc::new(NoopActivity),
            host: HostConfig::default(),
        })
        .with_agent_program("/usr/local/bin/mix".into());
    let mut machine = Machine::with(World::default(), performer);
    let real = profile_of(machine.real(&Query::Profile(user.clone())));
    let world = &mut machine.world;
    world
        .with_file(DEFAULT_PROFILE_NIX_ENV, b"nix-env", 0o555, ROOT_OWNER)
        .with_dir(&user.home, 0o700, owner)
        .with_dir(user.home.join(".local"), 0o755, owner)
        .with_dir(user.home.join(".local/state"), 0o755, owner)
        .with_dir(&state, 0o700, owner)
        .with_dir(state.join(".git"), 0o755, owner);
    for (file, contents) in [FLAKE_NIX, HOME_NIX, FLAKE_LOCK].iter().zip(&config) {
        world.with_file(
            state.join(file),
            contents.as_deref().unwrap_or_default(),
            0o644,
            owner,
        );
    }
    let last = *real
        .generations
        .iter()
        .max()
        .expect("bootstrap built a generation");
    world.profiles.insert(
        user.uid,
        Profile {
            generations: real.generations.clone(),
            active: real.active,
            built: BTreeMap::from([(last, config.clone())]),
        },
    );
    let home_nix = state.join(HOME_NIX);
    let changed: Arc<[u8]> = {
        let text = String::from_utf8(config[1].as_deref().unwrap().to_vec()).unwrap();
        let end = text.rfind('}').unwrap();
        Arc::from(
            format!(
                "{}  manual.manpages.enable = false;\n{}",
                &text[..end],
                &text[end..]
            )
            .as_bytes(),
        )
    };
    let activate = {
        let user = user.clone();
        move |_: &mut Machine, _: bool| Action::ActivateProfile {
            user: user.clone(),
            allow_source_builds: true,
        }
    };
    let change = {
        let home_nix = home_nix.clone();
        move |machine: &mut Machine, model: bool| Action::PutFile {
            path: home_nix.clone(),
            contents: changed.clone(),
            mode: 0o644,
            owner: None,
            expect: Expect::Present(
                observed(machine, model, Query::Path(home_nix.clone()), path_facts)
                    .id
                    .unwrap(),
            ),
        }
    };
    let back = {
        let user = user.clone();
        move |machine: &mut Machine, model: bool| {
            let now = observed(machine, model, Query::Profile(user.clone()), profile_of);
            Action::SwitchGeneration {
                user: user.clone(),
                generation: Some(last),
                expect: now.active,
            }
        }
    };
    let record = {
        let user = user.clone();
        move |_: &mut Machine, _: bool| Action::RecordState { user: user.clone() }
    };

    machine.run(
        &[&activate, &change, &activate, &back, &activate, &record],
        &[
            Query::Profile(user.clone()),
            Query::Contents(home_nix.clone()),
            Query::Path(state.join(".git")),
        ],
    );
}
