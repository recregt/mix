use std::future::Future;
use std::pin::Pin;

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::DEFAULT_PROFILE_NIX;
use mix_core::declared::pins::locked;
use mix_core::effect::Failure;
use mix_core::ops::update::Versions;
use mix_core::{Error, Result};
use mix_events::v1::{CheckedPackage, FetchPlan, PackageVerdict};
use mix_exec::Scope;
use mix_nixgen::lock::LockedInput;
use mix_nixgen::{Input, Pins, Rev};
use serde_json::Value;

use crate::effect::exec::run_as;
use crate::effect::git::Git;

pub type Newest<'a> = Pin<Box<dyn Future<Output = Result<Pins<LockedInput>>> + Send + 'a>>;

pub type Evaluated<'a> = Pin<Box<dyn Future<Output = Result<Versions>> + Send + 'a>>;

pub type Assessed<'a> = Pin<Box<dyn Future<Output = Result<Assessment>> + Send + 'a>>;

#[derive(Debug, Default)]
pub struct Assessment {
    pub packages: Vec<CheckedPackage>,
    pub plan: Option<FetchPlan>,
    pub refusal: Option<Error>,
}

impl Assessment {
    pub fn available(packages: &[String]) -> Self {
        Self {
            packages: packages
                .iter()
                .map(|name| CheckedPackage {
                    name: name.clone(),
                    verdict: PackageVerdict::Available as i32,
                    ..CheckedPackage::default()
                })
                .collect(),
            ..Self::default()
        }
    }
}

pub trait Upstream: Send + Sync {
    fn newest<'a>(&'a self, user: &'a InvokingUser, scope: &'a Scope) -> Newest<'a>;

    fn assess<'a>(
        &'a self,
        user: &'a InvokingUser,
        packages: &'a [String],
        mirror: Option<&'a str>,
        scope: &'a Scope,
    ) -> Assessed<'a>;

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

    fn assess<'a>(
        &'a self,
        user: &'a InvokingUser,
        packages: &'a [String],
        mirror: Option<&'a str>,
        scope: &'a Scope,
    ) -> Assessed<'a> {
        Box::pin(crate::effect::installed::assess(
            user, packages, mirror, scope,
        ))
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

pub fn remote(input: &Input) -> String {
    format!("https://github.com/{}/{}", input.owner, input.repo)
}

pub fn reference(input: &Input, rev: &Rev) -> String {
    format!("github:{}/{}/{}", input.owner, input.repo, rev.as_str())
}

pub fn head(listing: &str, branch: &str) -> Option<Rev> {
    let wanted = format!("refs/heads/{branch}");
    listing.lines().find_map(|line| {
        let (rev, name) = line.split_once('\t')?;
        (name == wanted).then(|| Rev::parse(rev).ok()).flatten()
    })
}

async fn newest_rev(user: &InvokingUser, input: &Input, scope: &Scope) -> Result<Rev> {
    let remote = remote(input);
    let Some(listing) = Git::of(user).branches(user, &remote, scope).await? else {
        return Err(Error::Effect(Box::new(Failure::Network { url: remote })));
    };
    head(&listing, input.branch).ok_or_else(|| Error::Command {
        command: "git ls-remote".to_string(),
        detail: format!("{remote} has no branch {}", input.branch),
    })
}

async fn prefetch(user: &InvokingUser, input: &Input, scope: &Scope) -> Result<LockedInput> {
    let rev = newest_rev(user, input, scope).await?;
    let reference = reference(input, &rev);
    let args = [
        "flake",
        "prefetch",
        "--json",
        "--flake-registry",
        "",
        &reference,
    ];
    let answer = run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await?;
    parse(input, &answer)
        .filter(|locked| locked.rev == rev)
        .ok_or_else(|| Error::Command {
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

    const BRANCHES: &str = include_str!("../../fixtures/git/ls-remote-branches.txt");

    #[test]
    fn each_input_is_looked_up_in_its_own_repository() {
        assert_eq!(
            remote(mix_nixgen::Pin::Nixpkgs.input()),
            "https://github.com/NixOS/nixpkgs"
        );
        assert_eq!(
            remote(mix_nixgen::Pin::HomeManager.input()),
            "https://github.com/nix-community/home-manager"
        );
    }

    #[test]
    fn an_input_is_fetched_at_the_revision_its_branch_points_to() {
        let rev = Rev::parse("2cd3234928f60bbff695a7ee56115ef7799c47cd").unwrap();
        assert_eq!(
            reference(mix_nixgen::Pin::HomeManager.input(), &rev),
            "github:nix-community/home-manager/2cd3234928f60bbff695a7ee56115ef7799c47cd"
        );
    }

    #[test]
    fn the_head_of_a_branch_is_read_from_what_git_lists() {
        assert_eq!(
            head(BRANCHES, mix_nixgen::Pin::HomeManager.input().branch),
            Some(Rev::parse("2cd3234928f60bbff695a7ee56115ef7799c47cd").unwrap())
        );
        assert_eq!(
            head(BRANCHES, "release-25.05"),
            Some(Rev::parse("44831a7eaba4360fb81f2acc5ea6de5fde90aaa3").unwrap())
        );
    }

    #[test]
    fn only_the_branch_with_exactly_that_name_counts() {
        assert!(BRANCHES.contains("\trefs/heads/update/master\n"));
        assert_eq!(
            head(BRANCHES, "update/master").map(|rev| rev.as_str().to_string()),
            Some("40e3b99d82cf8c56dfc18f45f448c406257b0462".to_string())
        );
        assert_eq!(head(BRANCHES, "aster"), None);
        assert_eq!(head(BRANCHES, "nixos-unstable"), None);
    }

    #[test]
    fn a_listing_that_is_not_a_branch_list_names_no_head() {
        assert_eq!(head("", "master"), None);
        assert_eq!(head("short\trefs/heads/master\n", "master"), None);
        assert_eq!(
            head(
                "2cd3234928f60bbff695a7ee56115ef7799c47cd refs/heads/master\n",
                "master"
            ),
            None
        );
    }
}
