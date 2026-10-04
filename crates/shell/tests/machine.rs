use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use mix_core::action::{
    Action, Expect, Fact, Kind as PathKind, Owner, PathFacts, ProfileFacts, Query, UserSpec,
    rollback_order,
};
use mix_core::identity::InvokingUser;
use mix_core::identity::{NIXBLD_GID, NIXBLD_GROUP, NIXBLD_HOME, NIXBLD_SHELL, NIXBLD_UID_BASE};
use mix_core::paths::{
    DEFAULT_PROFILE_NIX_ENV, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, NIX_DAEMON_SERVICE_SRC,
    NIX_DAEMON_SOCKET_SRC, NIX_TREE_MODE, NIX_TREE_PATHS, POLICY_FILE, STATE_FILE, mix_state_dir,
};
use mix_core::policy::Policy;
use mix_core::world::{Profile, World};
use mix_exec::Scope;
use mix_shell::drive::Performer;
use mix_shell::effect::files::Files;
use mix_shell::effect::generations::ProfileContext;

const ROOT_OWNER: Owner = (0, 0);

type Step<'a> = &'a dyn Fn(&mut Machine, bool) -> Action;

type OwnedStep = Box<dyn Fn(&mut Machine, bool) -> Action>;

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
            .block_on(
                self.performer
                    .observe(std::slice::from_ref(query), &mix_exec::Scope::root()),
            )
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

    fn same(&mut self, queries: &[Query], seen: fn(Fact) -> Fact, after: &str) {
        for query in queries {
            let (model, real) = self.both(query);
            assert_eq!(seen(model), seen(real), "{query:?} after {after}");
        }
    }

    fn run(&mut self, steps: &[Step<'_>], queries: &[Query]) {
        self.run_seeing(steps, queries, comparable);
    }

    fn run_seeing(&mut self, steps: &[Step<'_>], queries: &[Query], seen: fn(Fact) -> Fact) {
        let baseline: Vec<Fact> = queries.iter().map(|query| seen(self.real(query))).collect();
        let mut undos = (Vec::new(), Vec::new());
        for step in steps {
            let (model, real) = (step(self, true), step(self, false));
            let (on_model, on_real) = self.perform(&model, &real);
            undos.0.push(on_model);
            undos.1.push(on_real);
            self.same(queries, seen, &format!("{real:?}"));
        }
        for (model, real) in rollback_order(&undos.0)
            .into_iter()
            .zip(rollback_order(&undos.1))
        {
            self.perform(&model, &real);
            self.same(queries, seen, &format!("undoing with {real:?}"));
        }
        let after: Vec<Fact> = queries.iter().map(|query| seen(self.real(query))).collect();
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

#[allow(clippy::disallowed_methods)]
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
fn every_profile_action_and_its_undo_match_the_model() {
    let user = enrolled();
    let owner = (user.uid, user.gid);
    let state = mix_state_dir(&user.home);
    let config: Vec<Option<Arc<[u8]>>> = [FLAKE_NIX, HOME_NIX, FLAKE_LOCK, STATE_FILE]
        .iter()
        .map(|file| std::fs::read(state.join(file)).ok().map(Arc::from))
        .collect();
    let performer = Performer::new(Files::open(Path::new("/"), "differential").unwrap())
        .with_profile(ProfileContext {
            mirror: Policy::load(std::fs::read_to_string(POLICY_FILE).ok().as_deref())
                .mirror()
                .map(|mirror| mirror.url().to_string()),
        })
        .with_agent_program("/usr/local/bin/mix-daemon".into());
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
    for (file, contents) in [FLAKE_NIX, HOME_NIX, FLAKE_LOCK, STATE_FILE]
        .iter()
        .zip(&config)
    {
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
            dangling: Vec::new(),
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
            source: mix_core::action::FlakeSource::Git,
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

fn present(fact: Fact) -> Fact {
    match fact {
        Fact::Path(facts) => Fact::Path(PathFacts {
            kind: if facts.kind == PathKind::Missing {
                PathKind::Missing
            } else {
                PathKind::File
            },
            mode: 0,
            owner: ROOT_OWNER,
            id: None,
            digest: None,
            changed: None,
        }),
        other => comparable(other),
    }
}

#[test]
#[allow(clippy::disallowed_methods)]
fn installing_and_removing_the_runtime_match_the_model() {
    let mirror = std::env::var("MIX_DIFFERENTIAL_MIRROR").expect("the mirror's url");
    let runtime = mix_shell::ops::bootstrap::runtime(Some(&mirror)).unwrap();
    let mut machine = Machine::new(World::default());
    let skeleton: Vec<&'static str> = std::iter::once("/nix")
        .chain(NIX_TREE_PATHS.iter().copied())
        .collect();
    let create: Vec<OwnedStep> = skeleton
        .iter()
        .map(|path| {
            let path = *path;
            Box::new(move |_: &mut Machine, _: bool| Action::CreateDir {
                path: path.into(),
                mode: NIX_TREE_MODE,
                owner: None,
            }) as OwnedStep
        })
        .collect();
    let install = move |_: &mut Machine, _: bool| Action::InstallRuntime {
        url: runtime.url.clone(),
        sha256: runtime.sha256,
        size: runtime.size,
    };
    let mut steps: Vec<Step<'_>> = create
        .iter()
        .map(|step| step.as_ref() as Step<'_>)
        .collect();
    let group = |_: &mut Machine, _: bool| Action::AddGroup {
        name: NIXBLD_GROUP.into(),
        gid: NIXBLD_GID,
    };
    let builder = |_: &mut Machine, _: bool| {
        Action::AddUser(UserSpec {
            name: format!("{NIXBLD_GROUP}1"),
            uid: NIXBLD_UID_BASE + 1,
            gid: NIXBLD_GID,
            home: NIXBLD_HOME.into(),
            shell: NIXBLD_SHELL.into(),
            comment: "mix differential build user".into(),
            groups: vec![NIXBLD_GROUP.into()],
        })
    };
    steps.push(&group);
    steps.push(&builder);
    steps.push(&install);
    let queries: Vec<Query> = skeleton
        .iter()
        .chain(&[
            DEFAULT_PROFILE_NIX_ENV,
            NIX_DAEMON_SERVICE_SRC,
            NIX_DAEMON_SOCKET_SRC,
        ])
        .map(|path| Query::Path((*path).into()))
        .collect();

    machine.run_seeing(&steps, &queries, present);
}
