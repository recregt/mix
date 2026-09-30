//! Building and activating a user's home-manager profile.
//!
//! This is the machinery `mix bootstrap` uses to stand a profile up and `mix install` uses to
//! change one, so it lives below both of them rather than inside either: a command decides
//! *when* a profile is activated and what a failure should read like, and neither has to reach
//! into the other to do it.

use std::sync::Arc;

use mix_core::ActivityReporter;
use mix_core::paths::{
    DEFAULT_PROFILE_NIX, HOME_MANAGER_PROFILE_NAME, mix_state_dir, nix_profiles_dir,
};
use mix_core::privilege::InvokingUser;
use mix_exec::Scope;
use mix_nixgen::{AttrPath, FlakeRef, Installable};

use crate::HostConfig;
use crate::effect::exec::run_as_reporting;
use crate::effect::fs;
use crate::effect::git;
use crate::effect::mirror;
use crate::profile::Result;

fn nix_options(mirror: Option<&str>) -> Vec<String> {
    let Some(base) = mirror::filter_mirror(mirror) else {
        return Vec::new();
    };

    vec![
        "--override-input".to_string(),
        "nixpkgs".to_string(),
        mirror::nixpkgs_override(base),
        "--override-input".to_string(),
        "home-manager".to_string(),
        mirror::home_manager_override(base),
    ]
}

fn nix_build_args(installable: &str, profile_str: &str, options: &[String]) -> Vec<String> {
    let mut args = vec![
        "build".to_string(),
        installable.to_string(),
        "--no-link".to_string(),
        "--print-out-paths".to_string(),
        "--profile".to_string(),
        profile_str.to_string(),
        // Structured records instead of a redrawn text bar: what is downloaded and built can then
        // be counted rather than parsed out of prose.
        "--log-format".to_string(),
        "internal-json".to_string(),
    ];
    args.extend_from_slice(options);
    args
}

fn as_refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

pub async fn switch(
    user: &InvokingUser,
    mirror: Option<&str>,
    activity: &Arc<dyn ActivityReporter>,
    scope: &Scope,
) -> Result<String> {
    let flake_attr = Installable::new(
        FlakeRef::path(mix_state_dir(&user.home))
            .expect("an invoking user always has an absolute home"),
        AttrPath::new(["homeConfigurations", &user.name, "activationPackage"])
            .expect("an invoking user's name never holds a quote"),
    )
    .render();
    let profile = nix_profiles_dir(&user.home).join(HOME_MANAGER_PROFILE_NAME);
    let args = nix_build_args(
        &flake_attr,
        &profile.to_string_lossy(),
        &nix_options(mirror),
    );
    run_as_reporting(
        user,
        DEFAULT_PROFILE_NIX,
        &as_refs(&args),
        scope,
        Some(Arc::clone(activity)),
    )
    .await
}

pub async fn activate_generation(
    user: &InvokingUser,
    generation: &str,
    activity: &Arc<dyn ActivityReporter>,
    scope: &Scope,
) -> Result<()> {
    let activate = format!("{generation}/activate");
    run_as_reporting(user, &activate, &[], scope, Some(Arc::clone(activity))).await?;
    Ok(())
}

pub async fn record(user: &InvokingUser, host: &HostConfig, scope: &Scope) -> Result<()> {
    let state_dir = mix_state_dir(&user.home);
    let git = git::Git::resolve(user, host.git_binary.as_deref()).await;
    if !fs::exists(state_dir.join(".git")).await {
        git.init(user, &state_dir, scope).await?;
    }
    git.sync(user, &state_dir, scope).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIRROR: &str = "http://mirror.internal";

    #[test]
    fn nix_options_are_empty_when_no_mirror_is_set() {
        assert!(nix_options(None).is_empty());
    }

    #[test]
    fn a_mirror_only_redirects_the_pinned_inputs() {
        let args = nix_build_args("path:/state#x", "/profile", &nix_options(Some(MIRROR)));

        assert!(args.iter().any(|a| a == "nixpkgs"));
        assert!(args.iter().any(|a| a == "home-manager"));
        assert!(args.iter().any(
            |a| a.starts_with(&format!("tarball+{MIRROR}/nixpkgs-")) && a.contains("narHash=")
        ));
        assert!(args.iter().any(
            |a| a.starts_with(&format!("tarball+{MIRROR}/home-manager-")) && a.contains("narHash=")
        ));
    }

    #[test]
    fn a_user_never_hands_the_daemon_a_substituter_or_a_key() {
        for mirror in [None, Some(MIRROR)] {
            let args = nix_build_args("path:/state#x", "/profile", &nix_options(mirror));

            assert!(!args.iter().any(|a| a == "--option"), "{args:?}");
            assert!(!args.iter().any(|a| a.contains("substituters")), "{args:?}");
            assert!(
                !args.iter().any(|a| a.contains("trusted-public-keys")),
                "{args:?}"
            );
        }
    }

    #[test]
    fn nix_build_args_always_start_with_the_build_invocation() {
        for mirror in [None, Some(MIRROR)] {
            let args = nix_build_args("path:/state#x", "/profile", &nix_options(mirror));
            assert_eq!(
                args[..8],
                [
                    "build",
                    "path:/state#x",
                    "--no-link",
                    "--print-out-paths",
                    "--profile",
                    "/profile",
                    "--log-format",
                    "internal-json"
                ]
            );
        }
    }
}
