use mix_core::effect::{Fact, Query};
use mix_events::v1::{CleanRequest, CleanResult, node_finished};

use crate::Context;
use crate::profile::change;
use crate::request::{Concluded, Root};

pub(crate) async fn clean(ctx: &Context, root: &mut Root, request: &CleanRequest) -> Concluded {
    if ctx.caller_is_root {
        return root.refuse(change::Error::NotRoot);
    }
    let Some(cfg) = ctx.user.as_ref() else {
        return root.refuse(change::Error::NotBootstrapped);
    };
    let mut performer = match change::begin(ctx, root).await {
        Ok(performer) => performer,
        Err(concluded) => return *concluded,
    };
    let old = match performer
        .observe(&[Query::Profile(cfg.user.clone())], &ctx.scope)
        .await
        .as_deref()
    {
        Ok([Fact::Profile(profile)]) => mix_core::ops::change::old_generations(profile),
        _ => Vec::new(),
    };
    let before = request.all.then(|| ctx.host.available()).flatten();
    change::perform(
        ctx,
        root,
        performer,
        mix_core::ops::change::clean_steps(cfg, &ctx.policy, request.all, &ctx.request.id),
        || {
            node_finished::Result::Clean(CleanResult {
                generations: old,
                freed_bytes: before
                    .zip(ctx.host.available())
                    .map(|(b, a)| a.saturating_sub(b)),
            })
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use mix_events::v1::command::Request;

    use super::*;
    use crate::request::ran::{Ran, ran};

    async fn cleaned(session: crate::Session) -> Ran {
        ran(session, Request::Clean(CleanRequest { all: false })).await
    }

    #[tokio::test]
    async fn clean_needs_a_bootstrapped_user() {
        let ran = cleaned(crate::Session::new(mix_exec::Scope::root())).await;

        assert_eq!(ran.code(), Some(mix_events::v1::Code::NotBootstrapped));
    }
}
