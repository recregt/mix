use mix_core::report::diagnose::warning;
use mix_core::run::{Runner, StepSpec, Verdict};
use mix_events::ROOT;
use mix_events::v1::Code;

use crate::Context;
use crate::drive::drive;
use crate::ops::bootstrap::{Error, error_from};
use crate::request::{Concluded, Root};

pub(crate) async fn removal(
    ctx: &Context,
    root: &mut Root,
    steps: Vec<Box<dyn StepSpec>>,
) -> Result<Verdict, Box<Concluded>> {
    let mut performer = ctx.performer().map_err(|source| {
        Box::new(root.refuse(Error::Core(mix_core::Error::Io {
            path: "/".into(),
            source,
        })))
    })?;
    crate::ops::recover_interrupted(ctx, &mut root.tree, &mut performer, true).await;
    let mut runner = Runner::new(ROOT, steps);
    if ctx.dry_run {
        let report = drive(
            &mut runner,
            &mut root.tree,
            &mut performer,
            &ctx.scope,
            &root.stopped,
            &mut Vec::new(),
            &mut ctx.relay(),
        )
        .await;
        return Ok(report.verdict.clone());
    }
    let mut journal = ctx
        .journal(ctx.journals.as_path())
        .map_err(|failure| Box::new(root.refuse(error_from(failure))))?;
    let report = drive(
        &mut runner,
        &mut root.tree,
        &mut performer,
        &ctx.scope,
        &root.stopped,
        &mut journal,
        &mut ctx.relay(),
    )
    .await;
    let verdict = report.verdict.clone();
    if report.rollback_failures.is_empty()
        && let Err(failure) = journal.finish()
    {
        let _ = root.tree.warn(
            ROOT,
            warning(
                Code::CleanupIncomplete,
                "could not remove the finished journal",
                &failure,
            ),
        );
    }
    Ok(verdict)
}
