use mix_core::action::{Fact, Query};
use mix_core::paths::NIX_STORE;
use mix_events::v1::{CleanRequest, CleanResult, command, node_finished};

use crate::Context;
use crate::effect::generations;
use crate::profile::change::{self, Result};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub generations: Vec<u64>,
    /// Space the store gained, when `all` collected it.
    pub freed_bytes: Option<u64>,
}

fn available() -> Option<u64> {
    let stat = rustix::fs::statvfs(NIX_STORE).ok()?;
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

pub async fn clean(ctx: &Context, all: bool) -> Result<Cleaned> {
    let cfg = ctx.user.as_ref().ok_or(change::Error::NotBootstrapped)?;
    let old = match generations::observe(&Query::Profile(cfg.user.clone())) {
        Some(Fact::Profile(profile)) => mix_core::change::old_generations(&profile),
        _ => Vec::new(),
    };
    let before = all.then(available).flatten();
    let mut freed = None;
    change::perform(
        ctx,
        "clean",
        command::Request::Clean(CleanRequest { all }),
        mix_core::change::clean_steps(&cfg.user, all),
        || {
            freed = before.zip(available()).map(|(b, a)| a.saturating_sub(b));
            node_finished::Result::Clean(CleanResult {
                generations: old.clone(),
                freed_bytes: freed,
            })
        },
        &mut Vec::new(),
    )
    .await?;
    Ok(Cleaned {
        generations: old,
        freed_bytes: freed,
    })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::privilege::InvokingUser;

    use super::*;

    fn context(home: &std::path::Path) -> Context {
        Context::new(mix_exec::Scope::root()).with_user(Some(mix_core::models::UserConfig {
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

    #[tokio::test]
    async fn a_profile_with_no_generations_has_nothing_to_clean() {
        let home = tempfile::tempdir().unwrap();

        let cleaned = clean(&context(home.path()), false).await.unwrap();

        assert_eq!(cleaned, Cleaned::default());
    }

    #[tokio::test]
    async fn clean_needs_a_bootstrapped_user() {
        let ctx = Context::new(mix_exec::Scope::root());

        let refused = clean(&ctx, false).await.unwrap_err();

        assert!(matches!(refused, change::Error::NotBootstrapped));
    }
}
