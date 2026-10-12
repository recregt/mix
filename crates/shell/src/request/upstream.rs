use std::future::Future;
use std::pin::Pin;

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::DEFAULT_PROFILE_NIX;
use mix_core::declared::pins::locked;
use mix_core::ops::update::Versions;
use mix_core::{Error, Result};
use mix_exec::Scope;
use mix_nixgen::lock::LockedInput;
use mix_nixgen::{Input, Pins};
use serde_json::Value;

use crate::effect::exec::run_as;

pub type Newest<'a> = Pin<Box<dyn Future<Output = Result<Pins<LockedInput>>> + Send + 'a>>;

pub type Evaluated<'a> = Pin<Box<dyn Future<Output = Result<Versions>> + Send + 'a>>;

pub trait Upstream: Send + Sync {
    fn newest<'a>(&'a self, user: &'a InvokingUser, scope: &'a Scope) -> Newest<'a>;

    fn versions<'a>(
        &'a self,
        user: &'a InvokingUser,
        packages: &'a [String],
        pins: &'a Pins<LockedInput>,
        scope: &'a Scope,
    ) -> Evaluated<'a>;
}

pub struct Nix;

impl Upstream for Nix {
    fn newest<'a>(&'a self, user: &'a InvokingUser, scope: &'a Scope) -> Newest<'a> {
        Box::pin(async move {
            let (home_manager, nixpkgs) = tokio::join!(
                prefetch(user, mix_nixgen::Pin::HomeManager.input(), scope),
                prefetch(user, mix_nixgen::Pin::Nixpkgs.input(), scope)
            );
            Ok(Pins {
                home_manager: home_manager?,
                nixpkgs: nixpkgs?,
            })
        })
    }

    fn versions<'a>(
        &'a self,
        user: &'a InvokingUser,
        packages: &'a [String],
        pins: &'a Pins<LockedInput>,
        scope: &'a Scope,
    ) -> Evaluated<'a> {
        Box::pin(crate::effect::installed::versions_at(
            user, packages, pins, scope,
        ))
    }
}

pub fn reference(input: &Input) -> String {
    format!("github:{}/{}/{}", input.owner, input.repo, input.branch)
}

async fn prefetch(user: &InvokingUser, input: &Input, scope: &Scope) -> Result<LockedInput> {
    let reference = reference(input);
    let args = ["flake", "prefetch", "--refresh", "--json", &reference];
    let answer = run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await?;
    parse(input, &answer).ok_or_else(|| Error::Command {
        command: "nix flake prefetch".to_string(),
        detail: format!("its answer for {reference} could not be read"),
    })
}

pub fn parse(input: &Input, answer: &str) -> Option<LockedInput> {
    let value: Value = serde_json::from_str(answer).ok()?;
    let hash = value.get("hash")?;
    let mut found = value.get("locked")?.clone();
    let fields = found.as_object_mut()?;
    if fields
        .get("narHash")
        .is_some_and(|nar_hash| nar_hash != hash)
    {
        return None;
    }
    fields.insert("narHash".to_string(), hash.clone());
    locked(input, &found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NIXPKGS: &str = include_str!("../../fixtures/nix/prefetch-nixpkgs.json");
    const HOME_MANAGER: &str = include_str!("../../fixtures/nix/prefetch-home-manager.json");

    #[test]
    fn what_nix_prefetch_prints_is_read_as_the_pin_it_fetched() {
        use mix_nixgen::lock::EMBEDDED;
        assert_eq!(
            parse(mix_nixgen::Pin::Nixpkgs.input(), NIXPKGS),
            Some(EMBEDDED.nixpkgs)
        );
        assert_eq!(
            parse(mix_nixgen::Pin::HomeManager.input(), HOME_MANAGER),
            Some(EMBEDDED.home_manager)
        );
    }

    #[test]
    fn a_locked_nar_hash_must_agree_with_the_fetched_one() {
        let mut answer: Value = serde_json::from_str(NIXPKGS).unwrap();
        let input = mix_nixgen::Pin::Nixpkgs.input();
        answer["locked"]["narHash"] = answer["hash"].clone();
        assert!(parse(input, &answer.to_string()).is_some());
        answer["locked"]["narHash"] = "sha256-pD3qVlQ4mUCFoTWrYtxecpSitQMsgJvJIN0FUIDZoEU=".into();
        assert!(parse(input, &answer.to_string()).is_none());
    }

    #[test]
    fn an_answer_that_is_not_a_pin_is_not_read() {
        let input = mix_nixgen::Pin::Nixpkgs.input();
        assert!(parse(input, "").is_none());
        assert!(parse(input, "{}").is_none());
        assert!(parse(input, r#"{"hash": "x", "locked": {"rev": "short"}}"#).is_none());
        assert!(parse(mix_nixgen::Pin::HomeManager.input(), NIXPKGS).is_none());
    }

    #[test]
    fn each_input_follows_its_own_branch() {
        assert_eq!(
            reference(mix_nixgen::Pin::Nixpkgs.input()),
            "github:NixOS/nixpkgs/nixos-unstable"
        );
        assert_eq!(
            reference(mix_nixgen::Pin::HomeManager.input()),
            "github:nix-community/home-manager/master"
        );
    }
}
