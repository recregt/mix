use std::future::Future;
use std::pin::Pin;

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::DEFAULT_PROFILE_NIX;
use mix_core::declared::pins::locked;
use mix_core::{Error, Result};
use mix_exec::Scope;
use mix_nixgen::lock::LockedInput;
use mix_nixgen::{INPUTS, Input, Pins};
use serde_json::Value;

use crate::effect::exec::run_as;

pub type Newest<'a> = Pin<Box<dyn Future<Output = Result<Pins<LockedInput>>> + Send + 'a>>;

pub trait Upstream: Send + Sync {
    fn newest<'a>(&'a self, user: &'a InvokingUser, scope: &'a Scope) -> Newest<'a>;
}

pub struct Nix;

impl Upstream for Nix {
    fn newest<'a>(&'a self, user: &'a InvokingUser, scope: &'a Scope) -> Newest<'a> {
        Box::pin(async move {
            let (home_manager, nixpkgs) = tokio::join!(
                prefetch(user, input(mix_nixgen::Pin::HomeManager), scope),
                prefetch(user, input(mix_nixgen::Pin::Nixpkgs), scope)
            );
            Ok(Pins {
                home_manager: home_manager?,
                nixpkgs: nixpkgs?,
            })
        })
    }
}

fn input(pin: mix_nixgen::Pin) -> &'static Input {
    INPUTS
        .iter()
        .find(|input| input.pin == pin)
        .expect("every pin names an input")
}

fn reference(input: &Input) -> String {
    format!("github:{}/{}/{}", input.owner, input.repo, input.branch)
}

async fn prefetch(user: &InvokingUser, input: &Input, scope: &Scope) -> Result<LockedInput> {
    let reference = reference(input);
    let args = ["flake", "prefetch", "--refresh", "--json", &reference];
    let answer = run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await?;
    parse(&answer).ok_or_else(|| Error::Command {
        command: "nix flake prefetch".to_string(),
        detail: format!("its answer for {reference} could not be read"),
    })
}

fn parse(answer: &str) -> Option<LockedInput> {
    let value: Value = serde_json::from_str(answer).ok()?;
    locked(value.get("locked")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRINTED: &str = r#"{
      "hash": "sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk=",
      "locked": {
        "lastModified": 1789426278,
        "narHash": "sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk=",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "efe6f071ede9d21c37462d2d6682d5e670099684",
        "type": "github"
      },
      "original": {"owner": "NixOS", "ref": "nixos-unstable", "repo": "nixpkgs", "type": "github"},
      "storePath": "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source"
    }"#;

    #[test]
    fn what_nix_prefetch_prints_is_read_as_a_pin() {
        let pin = parse(PRINTED).unwrap();
        assert_eq!(pin.rev.as_str(), "efe6f071ede9d21c37462d2d6682d5e670099684");
        assert_eq!(pin.last_modified, 1_789_426_278);
    }

    #[test]
    fn an_answer_that_is_not_a_pin_is_not_read() {
        assert!(parse("").is_none());
        assert!(parse("{}").is_none());
        assert!(parse(r#"{"locked": {"rev": "short"}}"#).is_none());
    }

    #[test]
    fn each_input_follows_its_own_branch() {
        assert_eq!(
            reference(input(mix_nixgen::Pin::Nixpkgs)),
            "github:NixOS/nixpkgs/nixos-unstable"
        );
        assert_eq!(
            reference(input(mix_nixgen::Pin::HomeManager)),
            "github:nix-community/home-manager/master"
        );
    }
}
