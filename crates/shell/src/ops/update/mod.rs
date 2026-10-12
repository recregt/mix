use std::collections::BTreeMap;

use mix_core::declared::targets::UserConfig;
use mix_core::ops::update::{Decision, InputUpdate, PackageChange, decide, package_changes};
use mix_core::run::Verdict;
use mix_events::v1::{
    InstalledPackage, PackageUpdate, PinOutcome, PinUpdate, PinnedInput, UpdateRequest,
    UpdateResult, node_finished,
};
use mix_nixgen::Pins;
use mix_nixgen::lock::LockedInput;

use crate::Context;
use crate::profile::change::{self, Error};
use crate::profile::state::Settled;
use crate::profile::{current_pins, repinned};
use crate::request::{Concluded, Root};

fn pinned(input: &InputUpdate, locked: &LockedInput) -> PinnedInput {
    PinnedInput {
        name: input.name.to_string(),
        owner: input.owner.to_string(),
        repo: input.repo.to_string(),
        branch: input.branch.to_string(),
        rev: locked.rev.as_str().to_string(),
        nar_hash: locked.nar_hash.as_str().to_string(),
        last_modified: locked.last_modified,
    }
}

fn wire(
    decision: &Decision,
    packages: &[PackageChange],
    generation: Option<String>,
) -> UpdateResult {
    UpdateResult {
        inputs: decision
            .inputs
            .iter()
            .map(|input| PinUpdate {
                outcome: if input.moved() {
                    PinOutcome::Updated
                } else {
                    PinOutcome::Current
                } as i32,
                before: Some(pinned(input, &input.before)),
                after: Some(pinned(input, &input.after)),
            })
            .collect(),
        packages: packages
            .iter()
            .map(|package| PackageUpdate {
                name: package.name.clone(),
                from_version: package.from.clone(),
                to_version: package.to.clone(),
            })
            .collect(),
        generation,
    }
}

pub(crate) async fn update(ctx: &Context, root: &mut Root, _request: &UpdateRequest) -> Concluded {
    if ctx.caller_is_root {
        return root.refuse(Error::NotRoot);
    }
    let Some(cfg) = ctx.user.as_ref() else {
        return root.refuse(Error::NotBootstrapped);
    };
    if ctx.mirror().is_some() {
        return root.refuse(Error::Mirrored);
    }
    let mut performer = match change::begin(ctx, root).await {
        Ok(performer) => performer,
        Err(concluded) => return *concluded,
    };
    if let Settled::Newer(version) = change::settled(&mut performer, cfg, &ctx.scope).await {
        return root.refuse(Error::NewerState(version));
    }
    let have = current_pins(&cfg.user, &ctx.host, &ctx.pinned);
    let newest = match ctx.upstream.newest(&cfg.user, &ctx.scope).await {
        Ok(newest) => newest,
        Err(error) => return root.refuse(Error::Core(error)),
    };
    let decision = decide(&have, &newest);
    let mut steps = Vec::new();
    let mut before = None;
    if !decision.is_current() {
        let Some(repinned) = repinned(cfg, &newest) else {
            return root.refuse(Error::NotBootstrapped);
        };
        before = versions(ctx, cfg).await;
        steps = mix_core::ops::change::pin_steps(
            &repinned,
            &ctx.policy,
            decision.subject(),
            &ctx.request.id,
        );
    }
    let verdict = match change::run_steps(ctx, root, performer, steps).await {
        Ok(verdict) => verdict,
        Err(concluded) => return *concluded,
    };
    let after = if matches!(verdict, Verdict::Succeeded) && !ctx.dry_run && !decision.is_current() {
        versions(ctx, cfg).await
    } else {
        None
    };
    let packages = changed(before, after);
    change::conclude(root, verdict, || {
        remember(ctx, cfg, newest, &decision);
        node_finished::Result::Update(wire(&decision, &packages, generation(cfg)))
    })
}

fn by_name(packages: &[InstalledPackage]) -> BTreeMap<String, String> {
    packages
        .iter()
        .filter_map(|package| {
            package
                .version
                .clone()
                .map(|version| (package.name.clone(), version))
        })
        .collect()
}

async fn versions(ctx: &Context, cfg: &UserConfig) -> Option<BTreeMap<String, String>> {
    let listed = ctx
        .installed
        .listing(&cfg.user, ctx.mirror(), &ctx.scope)
        .await
        .ok()??;
    Some(by_name(&listed.packages))
}

fn changed(
    before: Option<BTreeMap<String, String>>,
    after: Option<BTreeMap<String, String>>,
) -> Vec<PackageChange> {
    match (before, after) {
        (Some(before), Some(after)) => package_changes(&before, &after),
        _ => Vec::new(),
    }
}

fn generation(cfg: &UserConfig) -> Option<String> {
    crate::effect::generations::active_generation(&cfg.user).map(|path| path.display().to_string())
}

fn remember(ctx: &Context, cfg: &UserConfig, newest: Pins<LockedInput>, decision: &Decision) {
    if !ctx.dry_run && !decision.is_current() {
        ctx.pinned.set(cfg.user.uid, newest);
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::sync::Arc;

    use super::*;

    use mix_core::declared::identity::InvokingUser;
    use mix_core::declared::paths::{FLAKE_LOCK, FLAKE_NIX, mix_state_dir};
    use mix_core::declared::policy::Policy;
    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, UpdateRequest, UpdateResult};
    use mix_nixgen::Rev;
    use mix_nixgen::lock::NarHash;

    use crate::profile::embedded;
    use crate::request::ran::{Ran, ran};
    use crate::request::upstream::{Newest, Upstream};

    struct Fixed(Option<Pins<LockedInput>>);

    impl Upstream for Fixed {
        fn newest<'a>(
            &'a self,
            _: &'a mix_core::declared::identity::InvokingUser,
            _: &'a mix_exec::Scope,
        ) -> Newest<'a> {
            Box::pin(async move {
                self.0.clone().ok_or_else(|| mix_core::Error::Command {
                    command: "nix flake prefetch".to_string(),
                    detail: "no route to host".to_string(),
                })
            })
        }
    }

    fn newer() -> Pins<LockedInput> {
        let locked = LockedInput {
            rev: Rev::parse("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
            nar_hash: NarHash::parse("sha256-pD3qVlQ4mUCFoTWrYtxecpSitQMsgJvJIN0FUIDZoEU=")
                .unwrap(),
            last_modified: 1_790_000_000,
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
            flake: "flake-content".to_string(),
            lock: "lock-content".to_string(),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    fn session(home: &std::path::Path, upstream: Option<Pins<LockedInput>>) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_user(Some(user_config(home)))
            .with_journals(home.join("journal"))
            .with_upstream(Arc::new(Fixed(upstream)))
    }

    fn updated(ran: &Ran) -> &UpdateResult {
        match ran.result() {
            Some(node_finished::Result::Update(updated)) => updated,
            other => panic!(
                "expected an update result, got {other:?} ({:?})",
                ran.root().diagnostic
            ),
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
    async fn pins_that_are_already_the_newest_change_nothing() {
        let home = tempfile::tempdir().unwrap();

        let ran = ran(session(home.path(), Some(embedded())), update()).await;

        let outcomes: Vec<(String, i32)> = updated(&ran)
            .inputs
            .iter()
            .map(|input| (input.after.clone().unwrap().name, input.outcome))
            .collect();
        assert_eq!(
            outcomes,
            vec![
                ("home-manager".to_string(), PinOutcome::Current as i32),
                ("nixpkgs".to_string(), PinOutcome::Current as i32),
            ]
        );
        assert!(updated(&ran).packages.is_empty());
        untouched(home.path());
    }

    #[tokio::test]
    async fn a_failed_lookup_fails_the_command_and_changes_nothing() {
        let home = tempfile::tempdir().unwrap();

        let ran = ran(session(home.path(), None), update()).await;

        assert_eq!(ran.code(), Some(Code::CommandFailed));
        untouched(home.path());
    }

    #[tokio::test]
    async fn a_machine_that_fetches_from_a_mirror_cannot_update() {
        let home = tempfile::tempdir().unwrap();
        let session = session(home.path(), Some(newer()))
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

    fn package(name: &str, version: Option<&str>) -> InstalledPackage {
        InstalledPackage {
            name: name.to_string(),
            version: version.map(str::to_string),
            ..InstalledPackage::default()
        }
    }

    fn versions_of(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
        Some(
            pairs
                .iter()
                .map(|(name, version)| (name.to_string(), version.to_string()))
                .collect(),
        )
    }

    #[test]
    fn a_listing_is_read_as_the_versions_of_the_packages_that_have_one() {
        let listed = [
            package("curl", Some("8.22.0")),
            package("lib", None),
            package("git", Some("2.55.0")),
        ];
        assert_eq!(
            Some(by_name(&listed)),
            versions_of(&[("curl", "8.22.0"), ("git", "2.55.0")])
        );
    }

    #[test]
    fn package_changes_are_reported_only_when_both_listings_were_read() {
        let before = versions_of(&[("qemu", "11.1.0")]);
        let after = versions_of(&[("qemu", "11.1.1")]);
        assert_eq!(changed(before.clone(), after.clone()).len(), 1);
        assert!(changed(None, after).is_empty());
        assert!(changed(before, None).is_empty());
        assert!(changed(None, None).is_empty());
    }
}
