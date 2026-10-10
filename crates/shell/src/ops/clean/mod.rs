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
        mix_core::ops::change::clean_steps(&cfg.user, request.all),
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
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::declared::identity::InvokingUser;
    use mix_events::v1::command::Request;

    use super::*;
    use crate::request::ran::{Ran, ran};

    fn context(home: &std::path::Path) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_journals(home.join("journal"))
            .with_user(Some(mix_core::declared::targets::UserConfig {
                user: InvokingUser {
                    uid: 1000,
                    gid: 1000,
                    name: "mix-user".to_string(),
                    home: home.to_path_buf(),
                },
                flake: String::new(),
                lock: String::new(),
                home: String::new(),
                restored_state: None,
            }))
    }

    async fn cleaned(session: crate::Session) -> Ran {
        ran(session, Request::Clean(CleanRequest { all: false })).await
    }

    #[tokio::test]
    async fn a_profile_with_no_generations_has_nothing_to_clean() {
        let home = tempfile::tempdir().unwrap();

        let ran = cleaned(context(home.path())).await;

        assert_eq!(
            ran.result(),
            Some(&node_finished::Result::Clean(CleanResult::default()))
        );
        assert_eq!(
            std::fs::read_dir(home.path().join("journal"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn clean_needs_a_bootstrapped_user() {
        let ran = cleaned(crate::Session::new(mix_exec::Scope::root())).await;

        assert_eq!(ran.code(), Some(mix_events::v1::Code::NotBootstrapped));
    }
}
