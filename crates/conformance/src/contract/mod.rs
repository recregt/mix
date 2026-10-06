pub mod accounts;
pub mod files;
pub mod units;

use mix_core::action::{Action, Fact, Failure, Outcome, PathFacts, Query};
use mix_exec::Scope;
use mix_shell::drive::Performer;

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the real performer")
}

pub fn perform(
    runtime: &tokio::runtime::Runtime,
    performer: &mut Performer,
    action: &Action,
) -> Outcome {
    let scope = Scope::root();
    let mut progress = |_| {};
    let mut prepared = |_: &[Action]| Ok(());
    runtime.block_on(performer.perform(action, &scope, &mut progress, &mut prepared))
}

pub fn observe(
    runtime: &tokio::runtime::Runtime,
    performer: &mut Performer,
    query: &Query,
) -> Fact {
    runtime
        .block_on(performer.observe(std::slice::from_ref(query), &Scope::root()))
        .unwrap_or_else(|failure| {
            panic!("the real performer could not observe {query:?}: {failure:?}")
        })
        .remove(0)
}

pub fn class(outcome: &Outcome) -> String {
    match outcome {
        Ok(_) => "done".to_string(),
        Err(Failure::Conflict { subject, .. }) => format!("conflict at {subject}"),
        Err(_) => "failed".to_string(),
    }
}

pub fn comparable(fact: Fact) -> Fact {
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
