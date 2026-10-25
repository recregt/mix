use mix_core::ops::update::{Decision, InputUpdate, Outcome, PackageChange, decide};
use mix_core::run::Verdict;
use mix_events::v1::{
    ActiveGeneration, InputOutcome, LockedRevision, PackageUpdate, UpdateRequest, UpdateResult,
    node_finished,
};
use mix_nixgen::lock::LockedInput;

use crate::Context;
use crate::profile::change::{self, Error};
use crate::profile::repinned;
use crate::profile::state::Settled;
use crate::request::{Concluded, Root};

fn revision(locked: &LockedInput) -> LockedRevision {
    LockedRevision {
        rev: locked.rev.as_str().to_string(),
        nar_hash: locked.nar_hash.as_str().to_string(),
        last_modified: Some(mix_events::Timestamp {
            seconds: i64::try_from(locked.last_modified)
                .expect("a pin is read only when its time fits a timestamp"),
            nanos: 0,
        }),
    }
}

fn outcome(outcome: Outcome) -> InputOutcome {
    match outcome {
        Outcome::Updated => InputOutcome::Updated,
        Outcome::Current => InputOutcome::Current,
        Outcome::Kept => InputOutcome::Kept,
    }
}

fn input(update: &InputUpdate) -> mix_events::v1::InputUpdate {
    let input = update.input();
    mix_events::v1::InputUpdate {
        name: input.name.to_string(),
        owner: input.owner.to_string(),
        repo: input.repo.to_string(),
        branch: input.branch.to_string(),
        outcome: outcome(update.outcome) as i32,
        from: Some(revision(&update.from)),
        to: Some(revision(&update.to)),
    }
}

fn wire(
    decision: &Decision,
    packages: &[PackageChange],
    active: Option<ActiveGeneration>,
) -> UpdateResult {
    UpdateResult {
        inputs: decision.inputs.iter().map(input).collect(),
        packages: packages
            .iter()
            .map(|package| PackageUpdate {
                name: package.name.clone(),
                from_version: package.from.clone(),
                to_version: package.to.clone(),
            })
            .collect(),
        active,
    }
}

pub(crate) async fn update(ctx: &Context, root: &mut Root, _request: &UpdateRequest) -> Concluded {
    let cfg = match change::profile_user(ctx) {
        Ok(cfg) => cfg,
        Err(error) => return root.refuse(error),
    };
    if ctx.mirror().is_some() {
        return root.refuse(Error::Mirrored);
    }
    let (mut performer, cfg) = match change::begin(ctx, root, cfg).await {
        Ok(begun) => begun,
        Err(concluded) => return *concluded,
    };
    let cfg = &cfg;
    let packages = match change::settled(&mut performer, cfg, &ctx.scope).await {
        Settled::Current { manifest, .. } => manifest.packages,
        Settled::Newer(version) => return root.refuse(Error::NewerState(version)),
    };
    let newest = match ctx.upstream.newest(&cfg.user, &ctx.scope).await {
        Ok(newest) => newest,
        Err(error) => return root.refuse(Error::Core(error)),
    };
    let decision = decide(&cfg.pins, &newest);
    let changes = if decision.is_current() {
        Vec::new()
    } else {
        let installed = match ctx.host.active_generation(&cfg.user, &ctx.scope).await {
            Ok(active) => active
                .as_ref()
                .and_then(crate::effect::generations::pins)
                .unwrap_or_else(|| cfg.pins.clone()),
            Err(failure) => {
                return root.refuse(Error::Core(crate::effect::core_error(
                    failure,
                    &cfg.user.home,
                )));
            }
        };
        let (before, after) = tokio::join!(
            ctx.upstream
                .versions(&cfg.user, &packages, &installed, &ctx.scope),
            ctx.upstream
                .versions(&cfg.user, &packages, &decision.pins, &ctx.scope),
        );
        match (before, after) {
            (Ok(before), Ok(after)) => mix_core::ops::update::package_changes(&before, &after),
            (Err(error), _) | (_, Err(error)) => return root.refuse(Error::Core(error)),
        }
    };
    let steps = mix_core::ops::change::pin_steps(
        &repinned(cfg, decision.pins.clone()),
        &ctx.policy,
        (!decision.is_current()).then(|| decision.subject()),
        &ctx.request.id,
    );
    let verdict = match change::run_steps(ctx, root, &mut performer, steps).await {
        Ok(verdict) => verdict,
        Err(concluded) => return *concluded,
    };
    let active = if ctx.dry_run || decision.is_current() || !matches!(verdict, Verdict::Succeeded) {
        None
    } else {
        match ctx.host.active_generation(&cfg.user, &ctx.scope).await {
            Ok(active) => active.as_ref().map(crate::effect::generations::wire),
            Err(failure) => {
                return root.refuse(Error::Core(crate::effect::core_error(
                    failure,
                    &cfg.user.home,
                )));
            }
        }
    };
    change::conclude(root, verdict, || {
        node_finished::Result::Update(wire(&decision, &changes, active))
    })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::sync::Arc;

    use super::*;

    use mix_core::declared::identity::InvokingUser;
    use mix_core::declared::paths::{FLAKE_LOCK, FLAKE_NIX, mix_state_dir};
    use mix_core::declared::policy::Policy;
    use mix_core::declared::targets::UserConfig;
    use mix_core::ops::update::Versions;
    use mix_events::v1::Code;
    use mix_events::v1::command::Request;
    use mix_nixgen::lock::{EMBEDDED, NarHash};
    use mix_nixgen::{Pins, Rev};

    use crate::request::ran::ran;
    use crate::request::upstream::{Checked, Evaluated, Newest, Upstream};

    struct Fixed {
        newest: Option<Pins<LockedInput>>,
        evaluates: bool,
    }

    impl Upstream for Fixed {
        fn newest<'a>(&'a self, _: &'a InvokingUser, _: &'a mix_exec::Scope) -> Newest<'a> {
            Box::pin(async move {
                self.newest.clone().ok_or_else(|| mix_core::Error::Command {
                    command: "nix flake prefetch".to_string(),
                    detail: "no route to host".to_string(),
                })
            })
        }

        fn check<'a>(
            &'a self,
            _: &'a InvokingUser,
            _: &'a [String],
            _: Option<&'a str>,
            _: &'a mix_exec::Scope,
        ) -> Checked<'a> {
            Box::pin(async { Ok(()) })
        }

        fn versions<'a>(
            &'a self,
            _: &'a InvokingUser,
            packages: &'a [String],
            pins: &'a Pins<LockedInput>,
            _: &'a mix_exec::Scope,
        ) -> Evaluated<'a> {
            Box::pin(async move {
                if !self.evaluates {
                    return Err(mix_core::Error::Command {
                        command: "nix eval".to_string(),
                        detail: "attribute 'gone' missing".to_string(),
                    });
                }
                Ok(packages
                    .iter()
                    .map(|name| {
                        (
                            name.clone(),
                            Some(pins.nixpkgs.rev.as_str()[..7].to_string()),
                        )
                    })
                    .collect::<Versions>())
            })
        }
    }

    fn newer() -> Pins<LockedInput> {
        let locked = LockedInput {
            rev: Rev::parse("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
            nar_hash: NarHash::parse("sha256-pD3qVlQ4mUCFoTWrYtxecpSitQMsgJvJIN0FUIDZoEU=")
                .unwrap(),
            last_modified: EMBEDDED.home_manager.last_modified + 1,
        };
        Pins {
            home_manager: locked.clone(),
            nixpkgs: locked,
        }
    }

    fn user_config(home: &std::path::Path) -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: home.to_path_buf(),
            },
            pins: EMBEDDED,
            flake: "flake-content".to_string(),
            lock: mix_nixgen::lock::render(EMBEDDED),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    fn session(home: &std::path::Path, upstream: Fixed) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_user(Some(user_config(home)))
            .with_journals(home.join("journal"))
            .with_upstream(Arc::new(upstream))
    }

    fn reachable(newest: Pins<LockedInput>) -> Fixed {
        Fixed {
            newest: Some(newest),
            evaluates: true,
        }
    }

    fn update() -> Request {
        Request::Update(UpdateRequest {})
    }

    fn untouched(home: &std::path::Path) {
        assert!(!mix_state_dir(home).join(FLAKE_NIX).exists());
        assert!(!mix_state_dir(home).join(FLAKE_LOCK).exists());
    }

    #[tokio::test]
    async fn a_failed_lookup_fails_the_command_and_changes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let unreachable = Fixed {
            newest: None,
            evaluates: true,
        };

        let ran = ran(session(home.path(), unreachable), update()).await;

        assert_eq!(ran.code(), Some(Code::CommandFailed));
        untouched(home.path());
    }

    #[tokio::test]
    async fn packages_that_cannot_be_evaluated_at_the_new_pins_fail_before_anything_changes() {
        let home = tempfile::tempdir().unwrap();
        let unevaluable = Fixed {
            newest: Some(newer()),
            evaluates: false,
        };

        let ran = ran(session(home.path(), unevaluable), update()).await;

        assert_eq!(ran.code(), Some(Code::CommandFailed));
        untouched(home.path());
    }

    #[tokio::test]
    async fn a_machine_that_fetches_from_a_mirror_cannot_update() {
        let home = tempfile::tempdir().unwrap();
        let session = session(home.path(), reachable(newer()))
            .with_policy(Policy::new(Some("http://mirror.internal"), None).unwrap());

        let ran = ran(session, update()).await;

        assert_eq!(ran.code(), Some(Code::UpdateUnavailable));
        untouched(home.path());
    }

    #[tokio::test]
    async fn a_user_who_was_never_set_up_is_told_to_bootstrap() {
        let session = crate::Session::new(mix_exec::Scope::root()).with_user(None);

        let ran = ran(session, update()).await;

        assert_eq!(ran.code(), Some(Code::NotBootstrapped));
    }

    #[tokio::test]
    async fn root_cannot_update_a_profile() {
        let session =
            crate::Session::new(mix_exec::Scope::root()).with_caller(crate::Caller::Account {
                peer_is_root: true,
                account: None,
            });

        let ran = ran(session, update()).await;

        assert_eq!(ran.code(), Some(Code::RootNotAllowed));
    }

    #[test]
    fn a_revision_is_written_with_its_hash_and_the_time_it_was_made() {
        let written = revision(&EMBEDDED.nixpkgs);
        assert_eq!(written.rev, EMBEDDED.nixpkgs.rev.as_str());
        assert_eq!(written.nar_hash, EMBEDDED.nixpkgs.nar_hash.as_str());
        assert_eq!(
            written.last_modified.unwrap().seconds,
            i64::try_from(EMBEDDED.nixpkgs.last_modified).unwrap()
        );
    }
}
