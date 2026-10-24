use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use mix_core::declared::identity::{
    InvokingUser, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_HOME, NIXBLD_SHELL, NIXBLD_UID_BASE,
};
use mix_core::declared::paths::{
    DEFAULT_PROFILE_NIX_ENV, GENERATION_INPUTS, HOME_NIX, NIX_DAEMON_SERVICE_SRC,
    NIX_DAEMON_SOCKET_SRC, NIX_TREE_MODE, NIX_TREE_PATHS, POLICY_FILE, REPOSITORY_CONFIG,
    REPOSITORY_CONFIG_CONTENTS, mix_state_dir,
};
use mix_core::declared::policy::Policy;
use mix_core::effect::{
    Action, Expect, Fact, Kind as PathKind, Owner, PathFacts, ProfileFacts, Query, UserSpec,
};
use mix_core::model::{Profile, World};
use mix_core::run::journal::rollback_order;
use mix_exec::Scope;
use mix_fs::Files;
use mix_shell::drive::Performer;
use mix_shell::effect::generations::ProfileContext;

use super::comparable;

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
            Performer::new(Files::open(Path::new("/"), super::request()).expect("the root opens")),
        )
    }

    fn with(world: World, performer: Performer) -> Self {
        Self {
            world,
            performer,
            runtime: super::runtime(),
        }
    }

    fn real(&mut self, query: &Query) -> Fact {
        super::observe(&self.runtime, &mut self.performer, query)
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
            let model = self.world.observe(query);
            let real = self.real(query);
            assert_eq!(seen(model), seen(real), "{query:?} after {after}");
        }
    }

    fn run(&mut self, steps: &[Step<'_>], queries: &[Query], seen: fn(Fact) -> Fact) -> u32 {
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
        u32::try_from(steps.len()).expect("a short sequence")
    }
}

fn observed<T>(machine: &mut Machine, model: bool, query: Query, pick: fn(Fact) -> T) -> T {
    pick(if model {
        machine.world.observe(&query)
    } else {
        machine.real(&query)
    })
}

fn path_facts(fact: Fact) -> PathFacts {
    match fact {
        Fact::Path(facts) => facts,
        other => panic!("a path query answered {other:?}"),
    }
}

fn profile_of(fact: Fact) -> ProfileFacts {
    match fact {
        Fact::Profile(profile) => profile,
        other => panic!("a profile query answered {other:?}"),
    }
}

#[allow(clippy::disallowed_methods)]
pub fn profile_actions(user: &InvokingUser) -> u32 {
    let owner = (user.uid, user.gid);
    let state = mix_state_dir(&user.home);
    let inputs: Vec<&str> = GENERATION_INPUTS.iter().map(|(file, _)| *file).collect();
    let config: Vec<Option<Arc<[u8]>>> = inputs
        .iter()
        .map(|file| std::fs::read(state.join(file)).ok().map(Arc::from))
        .collect();
    let performer =
        Performer::new(Files::open(Path::new("/"), super::request()).expect("the root opens"))
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
        .with_dir(state.join(".git"), 0o755, owner)
        .with_file(
            state.join(".git").join(REPOSITORY_CONFIG),
            REPOSITORY_CONFIG_CONTENTS.as_bytes(),
            0o644,
            owner,
        );
    for (file, contents) in inputs.iter().zip(&config) {
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
        let at = inputs
            .iter()
            .position(|file| *file == HOME_NIX)
            .expect("home.nix is a generation input");
        let text = String::from_utf8(
            config[at]
                .as_deref()
                .expect("bootstrap wrote home.nix")
                .to_vec(),
        )
        .expect("home.nix is text");
        let end = text.rfind('}').expect("home.nix ends its attribute set");
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
            source: mix_core::effect::FlakeSource::Git,
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
                    .expect("home.nix exists"),
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
        comparable,
    )
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

pub fn runtime_install(mirror: &str) -> u32 {
    let runtime = mix_shell::ops::bootstrap::runtime(Some(mirror))
        .unwrap_or_else(|failure| panic!("the runtime at {mirror}: {failure:?}"));
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
            comment: "mix contract build user".into(),
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

    machine.run(&steps, &queries, present)
}
