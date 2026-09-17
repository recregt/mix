//! Building and activating a user's home-manager profile.
//!
//! This is the machinery `mix bootstrap` uses to stand a profile up and `mix install` uses to
//! change one, so it lives below both of them rather than inside either: a command decides
//! *when* a profile is activated and what a failure should read like, and neither has to reach
//! into the other to do it.

use std::sync::Arc;

use mix_core::models::UserConfig;
use mix_core::nix_plan;
use mix_core::paths::{
    DEFAULT_PROFILE_NIX, HOME_MANAGER_PROFILE_NAME, mix_state_dir, nix_profiles_dir,
};
use mix_core::{ActivityReporter, CancellationToken};

use crate::exec::{plan_as, run_as, run_as_reporting};
use crate::fs;
use crate::git;
use crate::mirror;
use crate::profile::{Error, Result};

/// How many planned derivations are worth asking nix about.
///
/// A plan longer than this is a source build many times over, so reading the attributes of every
/// entry would mean parsing megabytes of JSON to reach an answer that is already known.
const CLASSIFY_LIMIT: usize = 64;

/// Whether a package the binary cache cannot provide may be compiled on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPolicy {
    /// Fetch binaries only, and refuse rather than start a compile nobody asked for.
    CacheOnly,
    /// Build whatever the cache has no binary for.
    AllowSource,
}

impl BuildPolicy {
    pub fn from_allowing_source(allow: bool) -> Self {
        if allow {
            Self::AllowSource
        } else {
            Self::CacheOnly
        }
    }
}

/// Where every nix invocation in this path fetches its inputs and its binaries from.
///
/// Resolved once and shared, so the mirror's key is fetched once no matter how many times nix is
/// called.
async fn nix_options(mirror: Option<&str>, mirror_key: Option<&str>) -> Vec<String> {
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
        "--option".to_string(),
        "substituters".to_string(),
        mirror::substituter(base),
        "--option".to_string(),
        "trusted-public-keys".to_string(),
        mirror::trusted_public_keys(base, mirror_key).await,
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

/// Asks what the build would entail, without building any of it.
///
/// Evaluating a home-manager configuration is the expensive half of an activation, seconds of
/// work either way, so the plan is not asked for on top of the build but ahead of it: nix keys
/// its evaluation cache on the flake's contents, and the build that follows this dry run reads
/// the evaluation back out of that cache instead of repeating it.
fn nix_dry_run_args(installable: &str, options: &[String]) -> Vec<String> {
    let mut args = vec![
        "build".to_string(),
        installable.to_string(),
        "--no-link".to_string(),
        "--dry-run".to_string(),
    ];
    args.extend_from_slice(options);
    args
}

fn nix_derivation_show_args(paths: &[&str]) -> Vec<String> {
    let mut args = vec!["derivation".to_string(), "show".to_string()];
    args.extend(paths.iter().map(|path| (*path).to_string()));
    args
}

fn as_refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Refuses an activation that would compile a package instead of fetching it.
///
/// home-manager's own generation is always built here — the profile is assembled on the machine
/// it is for, and no cache can hold it — so the plan is classified rather than merely counted:
/// only the entries nix would not have built locally anyway are a source build. The
/// documentation home-manager renders from its own sources is allowed by name, so a store that
/// has never built it — a clean install, or one after a garbage collection — installs as usual.
async fn refuse_source_builds(
    cfg: &UserConfig,
    installable: &str,
    options: &[String],
    token: &CancellationToken,
) -> Result<()> {
    let args = nix_dry_run_args(installable, options);
    let plan = plan_as(&cfg.user, DEFAULT_PROFILE_NIX, &as_refs(&args), token).await?;
    if plan.is_empty() {
        return Ok(());
    }

    // home-manager renders its own documentation here whatever a cache holds, and on a store
    // that has never built it — a clean install, or one after a garbage collection — it is
    // planned like anything else. Those entries are dropped before nix is asked about them, so a
    // clean install neither pays for their attributes nor counts them against the limit below.
    let candidates: Vec<&str> = plan
        .to_build()
        .iter()
        .map(String::as_str)
        .filter(|path| !nix_plan::is_always_local(nix_plan::derivation_name(path)))
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }

    let classified = candidates.len().min(CLASSIFY_LIMIT);
    let show = nix_derivation_show_args(&candidates[..classified]);
    let shown = run_as(&cfg.user, DEFAULT_PROFILE_NIX, &as_refs(&show), token).await?;

    let mut source_builds: Vec<String> = nix_plan::source_builds(&candidates[..classified], &shown)
        .into_iter()
        .map(str::to_string)
        .collect();
    source_builds.extend(candidates[classified..].iter().map(|p| {
        // Beyond the classification limit nothing is assumed: an unclassified build is refused.
        nix_plan::derivation_name(p).to_string()
    }));

    if source_builds.is_empty() {
        return Ok(());
    }
    Err(Error::SourceBuildRequired(source_builds))
}

pub async fn activate(
    cfg: &UserConfig,
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    activity: &Arc<dyn ActivityReporter>,
    token: &CancellationToken,
    policy: BuildPolicy,
) -> Result<bool> {
    let state_dir = mix_state_dir(&cfg.user.home);
    let state_dir_str = state_dir.to_string_lossy().into_owned();

    let flake_attr = format!(
        "path:{state_dir_str}#homeConfigurations.\"{}\".activationPackage",
        cfg.user.name
    );
    let profile = nix_profiles_dir(&cfg.user.home).join(HOME_MANAGER_PROFILE_NAME);
    let profile_str = profile.to_string_lossy().into_owned();
    let options = nix_options(mirror, mirror_key).await;

    if policy == BuildPolicy::CacheOnly {
        refuse_source_builds(cfg, &flake_attr, &options, token).await?;
    }

    let args = nix_build_args(&flake_attr, &profile_str, &options);
    let store_path = run_as_reporting(
        &cfg.user,
        DEFAULT_PROFILE_NIX,
        &as_refs(&args),
        token,
        Some(Arc::clone(activity)),
    )
    .await?;

    let activate = format!("{store_path}/activate");
    run_as_reporting(&cfg.user, &activate, &[], token, Some(Arc::clone(activity))).await?;

    let git = git::Git::resolve(&cfg.user).await;
    let created_git_dir = !fs::exists(state_dir.join(".git")).await;
    if created_git_dir {
        git.init(&cfg.user, &state_dir, token).await?;
    }
    git.sync(&cfg.user, &state_dir, token).await?;

    Ok(created_git_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNREACHABLE_MIRROR: &str = "http://127.0.0.1:1";

    #[tokio::test]
    async fn nix_options_are_empty_when_no_mirror_is_set() {
        assert!(nix_options(None, None).await.is_empty());
    }

    #[tokio::test]
    async fn nix_build_args_omits_mirror_flags_when_no_mirror_is_set() {
        let args = nix_build_args("path:/state#x", "/profile", &nix_options(None, None).await);
        assert!(!args.iter().any(|a| a == "--override-input"));
        assert!(!args.iter().any(|a| a == "substituters"));
    }

    #[tokio::test]
    async fn nix_build_args_adds_override_inputs_and_a_substituter_when_mirrored() {
        let args = nix_build_args(
            "path:/state#x",
            "/profile",
            &nix_options(Some(UNREACHABLE_MIRROR), None).await,
        );
        assert!(args.iter().any(|a| a == "nixpkgs"));
        assert!(args.iter().any(|a| a == "home-manager"));
        assert!(
            args.iter()
                .any(|a| a.starts_with(&format!("tarball+{UNREACHABLE_MIRROR}/nixpkgs-")))
        );
        assert!(
            args.iter()
                .any(|a| a.starts_with(&format!("tarball+{UNREACHABLE_MIRROR}/home-manager-")))
        );
        assert!(
            args.iter()
                .any(|a| a == &format!("{UNREACHABLE_MIRROR}/cache"))
        );
        assert!(args.iter().any(|a| a == "trusted-public-keys"));
        assert!(args.iter().any(|a| a.starts_with("cache.nixos.org-1:")));
    }

    #[tokio::test]
    async fn nix_build_args_always_start_with_the_build_invocation() {
        for mirror in [None, Some(UNREACHABLE_MIRROR)] {
            let args = nix_build_args(
                "path:/state#x",
                "/profile",
                &nix_options(mirror, None).await,
            );
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

    #[tokio::test]
    async fn nix_build_args_trusts_a_key_supplied_out_of_band() {
        const KEY: &str = "mix-mirror-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

        let args = nix_build_args(
            "path:/state#x",
            "/profile",
            &nix_options(Some(UNREACHABLE_MIRROR), Some(KEY)).await,
        );

        let keys = args
            .iter()
            .skip_while(|a| *a != "trusted-public-keys")
            .nth(1)
            .expect("the trusted-public-keys option is passed");
        assert!(
            keys.ends_with(KEY),
            "{keys:?} should end with the mirror key"
        );
    }

    #[tokio::test]
    async fn nix_build_args_ignores_a_blank_mirror() {
        let args = nix_build_args(
            "path:/state#x",
            "/profile",
            &nix_options(Some("   "), None).await,
        );
        assert!(!args.iter().any(|a| a == "--override-input"));
    }

    #[test]
    fn nix_dry_run_args_build_nothing_and_touch_no_profile() {
        let args = nix_dry_run_args("/nix/store/x.drv^*", &[]);

        assert_eq!(
            args,
            ["build", "/nix/store/x.drv^*", "--no-link", "--dry-run"]
        );
    }

    #[test]
    fn nix_derivation_show_args_name_every_planned_derivation() {
        let args = nix_derivation_show_args(&["/nix/store/a.drv", "/nix/store/b.drv"]);

        assert_eq!(
            args,
            ["derivation", "show", "/nix/store/a.drv", "/nix/store/b.drv"]
        );
    }

    /// The plan a clean store produces carries home-manager's own documentation, and none of it
    /// is worth asking nix about.
    #[test]
    fn the_documentation_a_clean_store_plans_is_not_asked_about() {
        let planned = [
            "/nix/store/00000000000000000000000000000001-options.json.drv",
            "/nix/store/00000000000000000000000000000002-hm-modules-messages.drv",
            "/nix/store/00000000000000000000000000000003-home-configuration-reference-manpage.drv",
            "/nix/store/00000000000000000000000000000004-hello-2.12.3.drv",
        ];
        let candidates: Vec<&str> = planned
            .into_iter()
            .filter(|path| !nix_plan::is_always_local(nix_plan::derivation_name(path)))
            .collect();

        assert_eq!(
            nix_derivation_show_args(&candidates),
            [
                "derivation",
                "show",
                "/nix/store/00000000000000000000000000000004-hello-2.12.3.drv"
            ]
        );
    }

    #[test]
    fn the_build_policy_follows_whether_source_builds_were_asked_for() {
        assert_eq!(
            BuildPolicy::from_allowing_source(true),
            BuildPolicy::AllowSource
        );
        assert_eq!(
            BuildPolicy::from_allowing_source(false),
            BuildPolicy::CacheOnly
        );
    }
}
