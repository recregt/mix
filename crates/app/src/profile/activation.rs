//! Building and activating a user's home-manager profile.
//!
//! This is the machinery `mix bootstrap` uses to stand a profile up and `mix install` uses to
//! change one, so it lives below both of them rather than inside either: a command decides
//! *when* a profile is activated and what a failure should read like, and neither has to reach
//! into the other to do it.

use std::sync::{Arc, OnceLock};

use mix_core::models::UserConfig;
use mix_core::nix_plan::{self, Approved};
use mix_core::paths::{
    DEFAULT_PROFILE_NIX, HOME_MANAGER_PROFILE_NAME, mix_state_dir, nix_profiles_dir,
};
use mix_core::{ActivityReporter, BuildProgress, CancellationToken};

use crate::exec::{plan_as, run_as_reporting, run_as_with_input};
use crate::fs;
use crate::git;
use crate::mirror;
use crate::profile::{Error, Result};

const DERIVATION_SHOW_ARGS: [&str; 3] = ["derivation", "show", "--stdin"];

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

fn as_refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

async fn refuse_source_builds(
    cfg: &UserConfig,
    installable: &str,
    options: &[String],
    token: &CancellationToken,
) -> Result<Approved> {
    let args = nix_dry_run_args(installable, options);
    let dry_run = plan_as(&cfg.user, DEFAULT_PROFILE_NIX, &as_refs(&args), token).await?;

    let spans = {
        let plan = match dry_run.plan() {
            Ok(plan) => plan,
            Err(error) => {
                tracing::info!("refusing a build plan nix printed in an unexpected shape: {error}");
                return Err(Error::SourceBuildRequired { packages: None });
            }
        };
        if plan.is_empty() {
            return Ok(Approved::none());
        }

        let shown = run_as_with_input(
            &cfg.user,
            DEFAULT_PROFILE_NIX,
            &DERIVATION_SHOW_ARGS,
            plan.to_build().join("\n").into_bytes(),
            token,
        )
        .await?;
        let classified = nix_plan::classify(plan.to_build(), &shown);

        if !classified.source.is_empty() {
            let packages = match classified.packages {
                Ok(packages) if !packages.is_empty() => {
                    Some(packages.into_iter().map(str::to_string).collect())
                }
                Ok(_) => None,
                Err(error) => {
                    tracing::info!("cannot name the packages behind a refused build: {error}");
                    None
                }
            };
            return Err(Error::SourceBuildRequired { packages });
        }

        classified
            .local
            .iter()
            .filter_map(|path| dry_run.span(path))
            .collect()
    };

    Ok(Approved::new(dry_run, spans))
}

struct SourceBuildGuard {
    inner: Arc<dyn ActivityReporter>,
    approved: Approved,
    refused: OnceLock<String>,
    token: CancellationToken,
}

impl ActivityReporter for SourceBuildGuard {
    fn line(&self, line: &str) {
        self.inner.line(line);
    }

    fn progress(&self, progress: &BuildProgress) {
        self.inner.progress(progress);
    }

    fn clear(&self) {
        self.inner.clear();
    }

    fn build_started(&self, derivation: &str) {
        if !self.approved.contains(derivation) && self.refused.set(derivation.to_string()).is_ok() {
            self.token.cancel();
        }
        self.inner.build_started(derivation);
    }
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

    let guard = match policy {
        BuildPolicy::CacheOnly => Some(Arc::new(SourceBuildGuard {
            inner: Arc::clone(activity),
            approved: refuse_source_builds(cfg, &flake_attr, &options, token).await?,
            refused: OnceLock::new(),
            token: token.child_token(),
        })),
        BuildPolicy::AllowSource => None,
    };

    let args = nix_build_args(&flake_attr, &profile_str, &options);
    let built = match &guard {
        Some(guard) => {
            run_as_reporting(
                &cfg.user,
                DEFAULT_PROFILE_NIX,
                &as_refs(&args),
                &guard.token,
                Some(Arc::clone(guard) as Arc<dyn ActivityReporter>),
            )
            .await
        }
        None => {
            run_as_reporting(
                &cfg.user,
                DEFAULT_PROFILE_NIX,
                &as_refs(&args),
                token,
                Some(Arc::clone(activity)),
            )
            .await
        }
    };
    if let Some(derivation) = guard.as_ref().and_then(|guard| guard.refused.get()) {
        tracing::info!("stopped a build the plan did not announce: {derivation}");
        return Err(Error::SourceBuildRequired { packages: None });
    }
    let store_path = built?;

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
    fn every_planned_derivation_is_asked_about_through_stdin() {
        assert_eq!(DERIVATION_SHOW_ARGS, ["derivation", "show", "--stdin"]);
    }

    #[derive(Default)]
    struct Recorded(std::sync::Mutex<Vec<String>>);

    impl ActivityReporter for Recorded {
        fn line(&self, line: &str) {
            self.0.lock().unwrap().push(line.to_string());
        }
        fn progress(&self, _progress: &BuildProgress) {}
        fn clear(&self) {}
        fn build_started(&self, derivation: &str) {
            self.0.lock().unwrap().push(format!("build {derivation}"));
        }
    }

    fn approved_set(paths: &[&str]) -> Approved {
        let dry_run = mix_core::nix_plan::DryRun::new(paths.join("\n"));
        let mut spans = Vec::new();
        let mut start = 0;
        for path in paths {
            let end = start + path.len();
            spans.push((start as u32, end as u32));
            start = end + 1;
        }
        Approved::new(dry_run, spans)
    }

    #[test]
    fn the_approved_set_finds_every_path_it_was_given_and_nothing_else() {
        let approved = approved_set(&[
            "/nix/store/c-c.drv",
            "/nix/store/a-a.drv",
            "/nix/store/b-b.drv",
        ]);

        assert_eq!(approved.len(), 3);
        for path in [
            "/nix/store/a-a.drv",
            "/nix/store/b-b.drv",
            "/nix/store/c-c.drv",
        ] {
            assert!(approved.contains(path), "{path}");
        }
        assert!(!approved.contains("/nix/store/d-d.drv"));
        assert!(!approved.contains("/nix/store/a-a"));
        assert!(!Approved::none().contains("/nix/store/a-a.drv"));
    }

    fn guard(approved: &[&str]) -> (SourceBuildGuard, Arc<Recorded>) {
        let recorded = Arc::new(Recorded::default());
        let guard = SourceBuildGuard {
            inner: Arc::clone(&recorded) as Arc<dyn ActivityReporter>,
            approved: approved_set(approved),
            refused: OnceLock::new(),
            token: CancellationToken::new(),
        };
        (guard, recorded)
    }

    #[test]
    fn the_guard_lets_an_approved_build_run() {
        let (guard, _) = guard(&["/nix/store/a-home-manager-path.drv"]);

        guard.build_started("/nix/store/a-home-manager-path.drv");

        assert!(!guard.token.is_cancelled());
        assert!(guard.refused.get().is_none());
    }

    #[test]
    fn the_guard_stops_a_build_the_plan_did_not_approve() {
        let (guard, _) = guard(&["/nix/store/a-home-manager-path.drv"]);

        guard.build_started("/nix/store/b-cowsay-3.8.4.drv");

        assert!(guard.token.is_cancelled());
        assert_eq!(
            guard.refused.get().map(String::as_str),
            Some("/nix/store/b-cowsay-3.8.4.drv")
        );
    }

    #[test]
    fn the_guard_stops_any_build_when_the_plan_approved_none() {
        let (guard, _) = guard(&[]);

        guard.build_started("/nix/store/a-home-manager-path.drv");

        assert!(guard.token.is_cancelled());
    }

    #[test]
    fn the_guard_keeps_the_first_build_it_stopped() {
        let (guard, _) = guard(&[]);

        guard.build_started("/nix/store/a-first.drv");
        guard.build_started("/nix/store/b-second.drv");

        assert_eq!(
            guard.refused.get().map(String::as_str),
            Some("/nix/store/a-first.drv")
        );
    }

    #[test]
    fn the_guard_passes_everything_through_to_the_screen() {
        let (guard, recorded) = guard(&["/nix/store/a.drv"]);

        guard.line("building");
        guard.build_started("/nix/store/a.drv");

        assert_eq!(
            *recorded.0.lock().unwrap(),
            ["building", "build /nix/store/a.drv"]
        );
    }

    #[test]
    fn stopping_a_build_does_not_cancel_the_run_it_belongs_to() {
        let parent = CancellationToken::new();
        let (mut guard, _) = guard(&[]);
        guard.token = parent.child_token();

        guard.build_started("/nix/store/a.drv");

        assert!(guard.token.is_cancelled());
        assert!(!parent.is_cancelled());
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
