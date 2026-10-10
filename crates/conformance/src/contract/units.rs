use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_core::declared::paths::SYSTEMD_UNIT_DIR;
use mix_core::effect::{Action, Expect, Fact, Query};
use mix_core::model::World;
use mix_core::run::journal::rollback_order;
use mix_shell::drive::Performer;
use mix_shell::effect::files::Files;
use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};

use super::files::Guess;
use super::{class, comparable, observe, perform, runtime};

pub const UNITS: [&str; 2] = ["mixc-a.service", "mixc-b.service"];
const CONTENTS: [&[u8]; 2] = [
    b"[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n\n[Install]\nWantedBy=multi-user.target\n",
    b"[Unit]\nDescription=mix contract\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n\n[Install]\nWantedBy=multi-user.target\n",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Install {
        unit: &'static str,
        contents: usize,
        expect: Guess,
    },
    Reload,
    Enable(&'static str),
    Disable(&'static str),
    Start(&'static str),
    Stop(&'static str),
    Restart(&'static str),
    Drain(&'static str),
}

fn file(unit: &str) -> PathBuf {
    Path::new(SYSTEMD_UNIT_DIR).join(unit)
}

fn concrete(op: &Op, ask: &mut dyn FnMut(&Query) -> Fact) -> Action {
    match op {
        Op::Install {
            unit,
            contents,
            expect,
        } => {
            let found = match ask(&Query::Path(file(unit))) {
                Fact::Path(facts) => facts.id,
                other => panic!("a path query answered {other:?}"),
            };
            Action::InstallUnit {
                unit: unit.to_string(),
                contents: Arc::from(CONTENTS[*contents]),
                expect: match (expect, found) {
                    (Guess::Current, Some(id)) => Expect::Present(id),
                    (Guess::Current, None) | (Guess::Absent, _) => Expect::Absent,
                    (Guess::Wrong, Some(_)) => Expect::Absent,
                    (Guess::Wrong, None) => Expect::Present(mix_core::effect::FileId {
                        dev: 0,
                        ino: u64::MAX,
                        born: None,
                    }),
                },
            }
        }
        Op::Reload => Action::DaemonReload,
        Op::Enable(unit) => Action::EnableUnit {
            unit: unit.to_string(),
        },
        Op::Disable(unit) => Action::DisableUnit {
            unit: unit.to_string(),
        },
        Op::Start(unit) => Action::StartUnit {
            unit: unit.to_string(),
        },
        Op::Stop(unit) => Action::StopUnit {
            unit: unit.to_string(),
        },
        Op::Restart(unit) => Action::RestartUnit {
            unit: unit.to_string(),
        },
        Op::Drain(unit) => Action::DrainService {
            unit: unit.to_string(),
        },
    }
}

fn queries() -> Vec<Query> {
    UNITS
        .iter()
        .flat_map(|unit| {
            [
                Query::Unit(unit.to_string()),
                Query::Path(file(unit)),
                Query::Contents(file(unit)),
            ]
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub world: World,
    pub undo: Vec<Vec<Action>>,
    pub outcome: Option<String>,
}

pub struct Units;

impl ReferenceStateMachine for Units {
    type State = Reference;
    type Transition = Op;

    fn init_state() -> BoxedStrategy<Reference> {
        Just(Reference {
            world: World::default(),
            undo: Vec::new(),
            outcome: None,
        })
        .boxed()
    }

    fn transitions(_: &Reference) -> BoxedStrategy<Op> {
        let unit = select(&UNITS[..]);
        let guess = prop_oneof![
            3 => Just(Guess::Current),
            1 => Just(Guess::Absent),
            1 => Just(Guess::Wrong),
        ];
        prop_oneof![
            2 => (unit.clone(), 0..CONTENTS.len(), guess).prop_map(|(unit, contents, expect)| {
                Op::Install {
                    unit,
                    contents,
                    expect,
                }
            }),
            2 => Just(Op::Reload),
            1 => unit.clone().prop_map(Op::Enable),
            1 => unit.clone().prop_map(Op::Disable),
            1 => unit.clone().prop_map(Op::Start),
            1 => unit.clone().prop_map(Op::Stop),
            1 => unit.clone().prop_map(Op::Restart),
            1 => unit.prop_map(Op::Drain),
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
        for unit in UNITS {
            self.act(&Action::StopUnit {
                unit: unit.to_string(),
            });
            self.act(&Action::DisableUnit {
                unit: unit.to_string(),
            });
            if let Fact::Path(facts) =
                observe(&self.runtime, &mut self.performer, &Query::Path(file(unit)))
                && let Some(id) = facts.id
            {
                self.act(&Action::RemoveCreated {
                    path: file(unit),
                    expect: id,
                });
            }
        }
        self.act(&Action::DaemonReload);
        let _ = mix_exec::Command::new("systemctl")
            .arg("reset-failed")
            .args(UNITS)
            .output_blocking(&mix_exec::Scope::root());
    }
}

fn same(world: &World, real: &mut Real, after: &str) {
    for query in queries() {
        let expected = comparable(world.observe(&query));
        let found = real.ask(&query);
        assert_eq!(expected, found, "{query:?} after {after}");
    }
}

impl StateMachineTest for Units {
    type SystemUnderTest = Real;
    type Reference = Units;

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
        }));
        real.clear();
        if let Err(panic) = undone {
            std::panic::resume_unwind(panic);
        }
    }
}
