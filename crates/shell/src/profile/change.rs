use mix_core::change::{Change, NewerList, Unrenderable};
use mix_core::plan::{Runner, StepSpec, Verdict, diagnostic};
use mix_core::targets::UserConfig;
use mix_events::v1::{InstallResult, RemoveResult, node_finished};
use mix_events::{Ending, ROOT};

use crate::Context;
use mix_events::v1::Code;

use crate::drive::drive;
use crate::effect::generations::ProfileContext;
use crate::effect::home::core_error;

use crate::profile::state::{self, Invalid, Settled, Source};
use crate::request::{Concluded, Root};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error(transparent)]
    InvalidPackage(#[from] mix_nixgen::InvalidInput),

    #[error("the package list would not be valid: {0}")]
    InvalidState(#[from] Invalid),

    #[error("the package list was written by a newer version of mix (format {0})")]
    NewerState(u32),

    #[error("this command changes the invoking user's profile, and it was run as root")]
    NotRoot,

    #[error("no managed environment was found for the invoking user")]
    NotBootstrapped,
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<Unrenderable> for Error {
    fn from(error: Unrenderable) -> Self {
        match error {
            Unrenderable::Package(error) => Error::InvalidPackage(error),
            Unrenderable::State(invalid) => Error::InvalidState(invalid),
        }
    }
}

impl From<NewerList> for Error {
    fn from(NewerList(version): NewerList) -> Self {
        Error::NewerState(version)
    }
}

pub fn settled(
    cfg: &UserConfig,
    host: &crate::request::context::Host,
    locked: &crate::request::Locked,
) -> Settled {
    state::settle(&cfg.user, host, locked)
}

pub enum Verb {
    Install,
    Remove,
}

impl Verb {
    fn doing(&self) -> mix_events::v1::Verb {
        match self {
            Verb::Install => mix_events::v1::Verb::Installing,
            Verb::Remove => mix_events::v1::Verb::Removing,
        }
    }

    fn result(&self, change: &Change) -> node_finished::Result {
        let reset = change.source == Source::Fresh;
        match self {
            Verb::Install => node_finished::Result::Install(InstallResult {
                added: change.changed.clone(),
                skipped: change.skipped.clone(),
                restored: reset,
            }),
            Verb::Remove => node_finished::Result::Remove(RemoveResult {
                removed: change.changed.clone(),
                skipped: change.skipped.clone(),
                restored: reset,
            }),
        }
    }
}

pub async fn run(
    ctx: &Context,
    root: &mut Root,
    cfg: &UserConfig,
    verb: Verb,
    change: &Change,
) -> Concluded {
    let steps = match mix_core::change::steps(&cfg.user, change, verb.doing()) {
        Ok(steps) => steps,
        Err(error) => return root.refuse(Error::from(error)),
    };
    perform(ctx, root, steps, || verb.result(change)).await
}

pub async fn perform(
    ctx: &Context,
    root: &mut Root,
    steps: Vec<Box<dyn StepSpec>>,
    result: impl FnOnce() -> node_finished::Result,
) -> Concluded {
    let verdict = if steps.is_empty() {
        Verdict::Succeeded
    } else {
        let performer = match ctx.performer() {
            Ok(performer) => performer,
            Err(source) => {
                return root.refuse(Error::Core(mix_core::Error::Io {
                    path: "/".into(),
                    source,
                }));
            }
        };
        let mut performer = performer.with_profile(ProfileContext {
            mirror: ctx.mirror().map(str::to_string),
        });
        let mut runner = Runner::new(ROOT, steps);
        if ctx.dry_run {
            let verdict = drive(
                &mut runner,
                &mut root.tree,
                &mut performer,
                &ctx.scope,
                &root.stopped,
                &mut Vec::new(),
                &mut ctx.relay(),
            )
            .await
            .verdict
            .clone();
            return conclude(root, verdict, result);
        }
        let mut journal = match ctx.journal(&ctx.journals) {
            Ok(journal) => journal,
            Err(failure) => return root.refuse(Error::Core(core_error(failure, &ctx.journals))),
        };
        let verdict = drive(
            &mut runner,
            &mut root.tree,
            &mut performer,
            &ctx.scope,
            &root.stopped,
            &mut journal,
            &mut ctx.relay(),
        )
        .await
        .verdict
        .clone();
        if let Err(failure) = journal.finish() {
            let _ = root.tree.warn(
                ROOT,
                mix_core::diagnose::warning(
                    Code::CleanupIncomplete,
                    "could not remove the finished journal",
                    &failure,
                ),
            );
        }
        verdict
    };
    conclude(root, verdict, result)
}

fn conclude(
    root: &mut Root,
    verdict: Verdict,
    result: impl FnOnce() -> node_finished::Result,
) -> Concluded {
    root.conclude(match verdict {
        Verdict::Succeeded => Ending::succeeded().with_result(result()),
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(&failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(cause),
    })
}

#[cfg(test)]
mod tests {
    use mix_core::state::StateManifest;

    use super::*;

    fn restored(source: Source) -> bool {
        let change = Change {
            changed: vec!["ripgrep".to_string()],
            skipped: Vec::new(),
            manifest: StateManifest::seed(),
            source,
        };
        match (Verb::Install.result(&change), Verb::Remove.result(&change)) {
            (node_finished::Result::Install(install), node_finished::Result::Remove(remove)) => {
                assert_eq!(install.restored, remove.restored);
                install.restored
            }
            other => panic!("expected an install and a remove result, got {other:?}"),
        }
    }

    #[test]
    fn only_a_package_list_that_could_not_be_recovered_is_reported_as_reset() {
        assert!(!restored(Source::File));
        assert!(!restored(Source::Generation));
        assert!(restored(Source::Fresh));
    }
}
