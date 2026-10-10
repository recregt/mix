use std::path::Path;

use mix_core::effect::{Action, Fact, GroupFacts, Owner, Query, UserFacts, UserSpec};
use mix_core::model::World;
use mix_core::run::journal::rollback_order;
use mix_shell::drive::Performer;
use mix_shell::effect::files::Files;
use proptest::prelude::*;
use proptest::sample::{select, subsequence};
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};

use super::files::Guess;
use super::{class, comparable, observe, perform, runtime};

pub const GROUPS: [&str; 2] = ["mixc-g0", "mixc-g1"];
pub const USERS: [&str; 2] = ["mixc-u0", "mixc-u1"];
const IDS: [u32; 4] = [39_000, 39_001, 39_002, 0];
const COMMENT: &str = "mix contract";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    AddGroup {
        group: &'static str,
        gid: u32,
    },
    SetGroupGid {
        group: &'static str,
        gid: u32,
        expect: Guess,
    },
    DeleteGroup {
        group: &'static str,
        expect: Guess,
    },
    AddUser {
        user: &'static str,
        uid: u32,
        gid: u32,
        groups: Vec<&'static str>,
    },
    SetUserIds {
        user: &'static str,
        ids: Owner,
        expect: Guess,
    },
    DeleteUser {
        user: &'static str,
        expect: Guess,
    },
    AddMember {
        group: &'static str,
        user: &'static str,
    },
    RemoveMember {
        group: &'static str,
        user: &'static str,
    },
}

fn group(fact: Fact) -> Option<GroupFacts> {
    match fact {
        Fact::Group(group) => group,
        other => panic!("a group query answered {other:?}"),
    }
}

fn user(fact: Fact) -> Option<UserFacts> {
    match fact {
        Fact::User(user) => user,
        other => panic!("a user query answered {other:?}"),
    }
}

fn concrete(op: &Op, ask: &mut dyn FnMut(&Query) -> Fact) -> Action {
    let gid_of = |found: Option<GroupFacts>, guess: Guess| match (guess, found) {
        (Guess::Current, Some(found)) => found.gid,
        (_, found) => found.map_or(1, |found| found.gid ^ 1),
    };
    let ids_of = |found: &Option<UserFacts>, guess: Guess| match (guess, found) {
        (Guess::Current, Some(found)) => (found.uid, found.gid),
        (_, found) => found
            .as_ref()
            .map_or((1, 1), |found| (found.uid ^ 1, found.gid)),
    };
    match op {
        Op::AddGroup { group, gid } => Action::AddGroup {
            name: group.to_string(),
            gid: *gid,
        },
        Op::SetGroupGid {
            group: name,
            gid,
            expect,
        } => Action::SetGroupGid {
            name: name.to_string(),
            gid: *gid,
            expect: gid_of(group(ask(&Query::Group(name.to_string()))), *expect),
        },
        Op::DeleteGroup {
            group: name,
            expect,
        } => Action::DeleteGroup {
            name: name.to_string(),
            expect: gid_of(group(ask(&Query::Group(name.to_string()))), *expect),
        },
        Op::AddUser {
            user,
            uid,
            gid,
            groups,
        } => Action::AddUser(UserSpec {
            name: user.to_string(),
            uid: *uid,
            gid: *gid,
            home: "/var/empty".into(),
            shell: "/usr/sbin/nologin".into(),
            comment: COMMENT.to_string(),
            groups: groups.iter().map(|group| group.to_string()).collect(),
        }),
        Op::SetUserIds {
            user: name,
            ids,
            expect,
        } => Action::SetUserIds {
            name: name.to_string(),
            ids: *ids,
            expect: ids_of(&user(ask(&Query::User(name.to_string()))), *expect),
        },
        Op::DeleteUser { user: name, expect } => {
            let found = user(ask(&Query::User(name.to_string())));
            Action::DeleteUser {
                name: name.to_string(),
                expect: ids_of(&found, *expect),
                comment: found.map(|found| found.comment).unwrap_or_default(),
            }
        }
        Op::AddMember { group, user } => Action::AddMember {
            group: group.to_string(),
            user: user.to_string(),
        },
        Op::RemoveMember { group, user } => Action::RemoveMember {
            group: group.to_string(),
            user: user.to_string(),
        },
    }
}

fn queries() -> Vec<Query> {
    GROUPS
        .iter()
        .map(|name| Query::Group(name.to_string()))
        .chain(USERS.iter().map(|name| Query::User(name.to_string())))
        .collect()
}

fn database(path: &str) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| line.split(':').map(str::to_string).collect())
        .collect()
}

pub fn seeded() -> World {
    let mut world = World::default();
    for fields in database("/etc/group") {
        if let [name, _, gid, members, ..] = fields.as_slice()
            && let Ok(gid) = gid.parse()
        {
            world.groups.insert(
                name.clone(),
                GroupFacts {
                    gid,
                    members: members
                        .split(',')
                        .filter(|member| !member.is_empty())
                        .map(str::to_string)
                        .collect(),
                },
            );
        }
    }
    for fields in database("/etc/passwd") {
        if let [name, _, uid, gid, comment, home, shell, ..] = fields.as_slice()
            && let (Ok(uid), Ok(gid)) = (uid.parse(), gid.parse())
        {
            world.users.insert(
                name.clone(),
                UserFacts {
                    uid,
                    gid,
                    home: home.into(),
                    shell: shell.into(),
                    comment: comment.clone(),
                },
            );
        }
    }
    world
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub world: World,
    pub undo: Vec<Vec<Action>>,
    pub outcome: Option<String>,
}

pub struct Accounts;

impl ReferenceStateMachine for Accounts {
    type State = Reference;
    type Transition = Op;

    fn init_state() -> BoxedStrategy<Reference> {
        Just(Reference {
            world: seeded(),
            undo: Vec::new(),
            outcome: None,
        })
        .boxed()
    }

    fn transitions(_: &Reference) -> BoxedStrategy<Op> {
        let group = select(&GROUPS[..]);
        let user = select(&USERS[..]);
        let id = select(&IDS[..]);
        let guess = prop_oneof![3 => Just(Guess::Current), 1 => Just(Guess::Wrong)];
        prop_oneof![
            (group.clone(), id.clone()).prop_map(|(group, gid)| Op::AddGroup { group, gid }),
            (group.clone(), id.clone(), guess.clone())
                .prop_map(|(group, gid, expect)| { Op::SetGroupGid { group, gid, expect } }),
            (group.clone(), guess.clone())
                .prop_map(|(group, expect)| Op::DeleteGroup { group, expect }),
            (
                user.clone(),
                id.clone(),
                id.clone(),
                subsequence(&GROUPS[..], 0..=GROUPS.len())
            )
                .prop_map(|(user, uid, gid, groups)| Op::AddUser {
                    user,
                    uid,
                    gid,
                    groups,
                }),
            (user.clone(), (id.clone(), id), guess.clone())
                .prop_map(|(user, ids, expect)| Op::SetUserIds { user, ids, expect }),
            (user.clone(), guess).prop_map(|(user, expect)| Op::DeleteUser { user, expect }),
            (group.clone(), user.clone()).prop_map(|(group, user)| Op::AddMember { group, user }),
            (group, user).prop_map(|(group, user)| Op::RemoveMember { group, user }),
        ]
        .boxed()
    }

    fn apply(mut state: Reference, op: &Op) -> Reference {
        let world = state.world.clone();
        let action = concrete(op, &mut |query| world.observe(query));
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
    performer: Performer,
    undo: Vec<Vec<Action>>,
}

impl Real {
    fn ask(&mut self, query: &Query) -> Fact {
        comparable(observe(&self.runtime, &mut self.performer, query))
    }

    fn act(&mut self, action: &Action) -> String {
        class(&perform(&self.runtime, &mut self.performer, action))
    }

    fn clear(&mut self) {
        for name in USERS {
            if let Some(found) = user(self.ask(&Query::User(name.to_string()))) {
                self.act(&Action::DeleteUser {
                    name: name.to_string(),
                    expect: (found.uid, found.gid),
                    comment: found.comment,
                });
            }
        }
        for name in GROUPS {
            if let Some(found) = group(self.ask(&Query::Group(name.to_string()))) {
                self.act(&Action::DeleteGroup {
                    name: name.to_string(),
                    expect: found.gid,
                });
            }
        }
    }
}

fn same(world: &World, real: &mut Real, after: &str) {
    for query in queries() {
        let expected = comparable(world.observe(&query));
        let found = real.ask(&query);
        assert_eq!(expected, found, "{query:?} after {after}");
    }
}

impl StateMachineTest for Accounts {
    type SystemUnderTest = Real;
    type Reference = Accounts;

    fn init_test(state: &Reference) -> Real {
        let mut real = Real {
            runtime: runtime(),
            performer: Performer::new(
                Files::open(Path::new("/"), super::files::REQUEST).expect("the root opens"),
            ),
            undo: Vec::new(),
        };
        real.clear();
        same(&state.world, &mut real, "starting");
        real
    }

    fn apply(mut real: Real, state: &Reference, op: Op) -> Real {
        let action = {
            let runtime = &real.runtime;
            let performer = &mut real.performer;
            concrete(&op, &mut |query| observe(runtime, performer, query))
        };
        let outcome = perform(&real.runtime, &mut real.performer, &action);
        if let Ok(performed) = &outcome {
            real.undo.push(performed.undo.clone());
        }
        assert_eq!(
            state.outcome.as_deref().unwrap_or_default(),
            class(&outcome),
            "{action:?}: the model and the machine disagree"
        );
        same(&state.world, &mut real, &format!("{action:?}"));
        real
    }

    fn teardown(mut real: Real, mut state: Reference) {
        let model = rollback_order(&state.undo);
        let machine = rollback_order(&real.undo);
        let undone = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_eq!(model.len(), machine.len());
            for (on_model, on_machine) in model.iter().zip(&machine) {
                let predicted = class(&state.world.apply(on_model));
                let outcome = real.act(on_machine);
                assert_eq!(
                    predicted, outcome,
                    "undoing with {on_machine:?}: the model and the machine disagree"
                );
                same(
                    &state.world,
                    &mut real,
                    &format!("undoing with {on_machine:?}"),
                );
            }
            for query in queries() {
                assert!(
                    matches!(real.ask(&query), Fact::Group(None) | Fact::User(None)),
                    "undoing everything left {query:?}"
                );
            }
        }));
        real.clear();
        if let Err(panic) = undone {
            std::panic::resume_unwind(panic);
        }
    }
}
