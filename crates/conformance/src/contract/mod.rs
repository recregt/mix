pub mod accounts;
pub mod files;
pub mod git;
pub mod units;

use mix_core::effect::{Action, Fact, Failure, Outcome, PathFacts, Query};
use mix_exec::Scope;
use mix_shell::drive::Performer;

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the real performer")
}

thread_local! {
    static FAILED: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

pub fn perform(
    runtime: &tokio::runtime::Runtime,
    performer: &mut Performer,
    action: &Action,
) -> Outcome {
    let scope = Scope::root();
    let mut progress = |_| {};
    let mut prepared = |_: &[Action]| Ok(());
    let outcome = runtime.block_on(performer.perform(action, &scope, &mut progress, &mut prepared));
    remember(&outcome);
    outcome
}

pub fn remember(outcome: &Outcome) {
    FAILED.with(|failed| {
        *failed.borrow_mut() = match outcome {
            Ok(_) => String::new(),
            Err(failure) => format!("{failure:?}"),
        }
    });
}

pub fn request() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "contract-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

pub fn failed() -> String {
    FAILED.with(|failed| failed.borrow().clone())
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
