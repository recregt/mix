use mix_core::declared::identity::{InvokingUser, MIX_USERS_GROUP};
use mix_core::declared::paths::mix_state_dir;
use mix_core::effect::{Fact, Failure, Query};
use mix_core::report::diagnose::warning;
use mix_core::run::{Runner, StepSpec, Verdict};
use mix_events::ROOT;
use mix_events::v1::Code;
use mix_exec::Scope;

use crate::Context;
use crate::drive::{Performer, drive};
use crate::ops::bootstrap::{Error, error_from};
use crate::request::{Concluded, Root};

pub(crate) async fn removal<T>(
    ctx: &Context,
    root: &mut Root,
    plan: impl FnOnce(Vec<InvokingUser>) -> (Vec<Box<dyn StepSpec>>, T),
) -> Result<(Verdict, T), Box<Concluded>> {
    let mut performer = ctx.performer().map_err(|source| {
        Box::new(root.refuse(Error::Core(mix_core::Error::Io {
            path: "/".into(),
            source,
        })))
    })?;
    crate::ops::recover_interrupted(ctx, &mut root.tree, &mut performer, true).await;
    let holders = holders(ctx, &mut performer)
        .await
        .map_err(|failure| Box::new(root.refuse(error_from(failure))))?;
    let (steps, planned) = plan(holders);
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
        return Ok((report.verdict.clone(), planned));
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
    Ok((verdict, planned))
}

async fn holders(ctx: &Context, performer: &mut Performer) -> Result<Vec<InvokingUser>, Failure> {
    let mut holders = enrolled(performer, &ctx.scope).await?;
    let others: Vec<InvokingUser> = ctx
        .host
        .regular_accounts()
        .into_iter()
        .filter(|account| !holders.iter().any(|holder| holder.name == account.name))
        .collect();
    let states: Vec<Query> = others
        .iter()
        .map(|account| Query::Path(mix_state_dir(&account.home)))
        .collect();
    let found = performer.observe(&states, &ctx.scope).await?;
    holders.extend(
        others
            .into_iter()
            .zip(found)
            .filter(|(_, fact)| matches!(fact, Fact::Path(facts) if facts.id.is_some()))
            .map(|(account, _)| account),
    );
    Ok(holders)
}

async fn enrolled(performer: &mut Performer, scope: &Scope) -> Result<Vec<InvokingUser>, Failure> {
    let group = performer
        .observe(&[Query::Group(MIX_USERS_GROUP.to_string())], scope)
        .await?;
    let Some(Fact::Group(Some(group))) = group.into_iter().next() else {
        return Ok(Vec::new());
    };
    let accounts: Vec<Query> = group
        .members
        .iter()
        .map(|member| Query::User(member.clone()))
        .collect();
    let found = performer.observe(&accounts, scope).await?;
    Ok(group
        .members
        .into_iter()
        .zip(found)
        .filter_map(|(name, fact)| match fact {
            Fact::User(Some(account)) => Some(InvokingUser {
                uid: account.uid,
                gid: account.gid,
                name,
                home: account.home,
            }),
            _ => None,
        })
        .collect())
}
