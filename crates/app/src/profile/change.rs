use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use mix_core::models::UserConfig;
use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
use mix_core::state::StateManifest;
use mix_core::{ActivityReporter, CancellationToken};
use tracing::Instrument;

use crate::fs::{remove_file, write_atomic};
use crate::profile::config::render_home;
use crate::profile::state::{self, Invalid, Settled, Source};
use crate::profile::{self, BuildPolicy};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error(transparent)]
    Activation(#[from] profile::Error),

    #[error(transparent)]
    InvalidPackage(#[from] mix_nixgen::InvalidInput),

    #[error("the package list would not be valid: {0}")]
    InvalidState(#[from] Invalid),

    #[error("the package list was written by a newer version of mix (format {0})")]
    NewerState(u32),

    #[error("this command changes the invoking user's profile, and it was run as root")]
    NotRoot,

    #[error("no managed environment was found for the invoking user")]
    NotBootstrapped,
}

pub type Result<T> = std::result::Result<T, Error>;

pub async fn settled(cfg: &UserConfig) -> Result<(StateManifest, Option<Source>)> {
    match state::settle(&cfg.user.home) {
        Settled::Newer(version) => Err(Error::NewerState(version)),
        Settled::Current {
            manifest,
            source: Source::File,
        } => Ok((manifest, None)),
        Settled::Current { manifest, source } => {
            let (new_state, new_home) = render_candidate(cfg, &manifest)?;
            let state_dir = mix_state_dir(&cfg.user.home);
            write_atomic(&state_dir.join(STATE_FILE), &new_state).await?;
            write_atomic(&state_dir.join(HOME_NIX), &new_home).await?;
            Ok((manifest, Some(source)))
        }
    }
}

pub async fn apply(
    cfg: &UserConfig,
    manifest: &StateManifest,
    label: &str,
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    activity: &Arc<dyn ActivityReporter>,
    policy: BuildPolicy,
) -> Result<()> {
    let (new_state, new_home) = render_candidate(cfg, manifest)?;

    let state_dir = mix_state_dir(&cfg.user.home);
    let state_path = state_dir.join(STATE_FILE);
    let home_path = state_dir.join(HOME_NIX);
    let token = CancellationToken::new();
    let span = tracing::info_span!("step", name = label);

    async {
        let generation = write_then_switch(&state_path, &home_path, &new_state, &new_home, || {
            profile::switch(cfg, mirror, mirror_key, activity, &token, policy)
        })
        .await?;
        profile::finish(cfg, &generation, activity, &token).await?;
        Ok(())
    }
    .instrument(span)
    .await
}

pub fn label(verb: &str, packages: &[String]) -> String {
    match packages.len() {
        0..=3 => format!("{verb} {}", packages.join(", ")),
        n => format!("{verb} {n} packages"),
    }
}

fn render_candidate(cfg: &UserConfig, manifest: &StateManifest) -> Result<(String, String)> {
    let home = render_home(&cfg.user, &manifest.packages)?;
    let rendered = manifest.render();
    state::validate(&rendered)?;
    Ok((rendered, home))
}

async fn write_then_switch<F, Fut>(
    state_path: &Path,
    home_path: &Path,
    new_state: &str,
    new_home: &str,
    switch: F,
) -> Result<String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = profile::Result<String>>,
{
    let previous_state = tokio::fs::read_to_string(state_path).await.ok();
    let previous_home = tokio::fs::read_to_string(home_path).await.ok();

    write_atomic(state_path, new_state).await?;
    write_atomic(home_path, new_home).await?;

    match switch().await {
        Ok(generation) => Ok(generation),
        Err(e) => {
            put_back(state_path, previous_state.as_deref()).await;
            put_back(home_path, previous_home.as_deref()).await;
            Err(e.into())
        }
    }
}

async fn put_back(path: &Path, previous: Option<&str>) {
    let restored = match previous {
        Some(previous) => write_atomic(path, previous).await,
        None => remove_file(path).await,
    };
    if let Err(error) = restored {
        tracing::info!("could not put {} back as it was: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use mix_core::privilege::InvokingUser;

    use super::*;

    fn user_config(home: &Path) -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: home.to_path_buf(),
            },
            flake: "flake-content".to_string(),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    fn manifest(version: u32, packages: &[&str]) -> StateManifest {
        StateManifest {
            version,
            packages: packages.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn label_names_a_short_list_of_packages() {
        assert_eq!(
            label("Installing", &["git".to_string(), "fd".to_string()]),
            "Installing git, fd"
        );
    }

    #[test]
    fn label_counts_a_long_list_of_packages() {
        let packages: Vec<String> = (0..12).map(|i| format!("package-{i}")).collect();

        assert_eq!(label("Removing", &packages), "Removing 12 packages");
    }

    #[test]
    fn render_candidate_renders_every_package_into_the_manifest_and_home_nix() {
        let home = tempfile::tempdir().unwrap();

        let (state, home_nix) =
            render_candidate(&user_config(home.path()), &manifest(1, &["git", "ripgrep"])).unwrap();

        assert_eq!(
            StateManifest::parse(&state).unwrap().packages,
            vec!["git".to_string(), "ripgrep".to_string()]
        );
        assert!(home_nix.contains("ripgrep"));
        assert!(home_nix.contains("git"));
    }

    #[test]
    fn render_candidate_refuses_a_format_it_does_not_write() {
        let home = tempfile::tempdir().unwrap();

        let err = render_candidate(&user_config(home.path()), &manifest(7, &["git"])).unwrap_err();

        assert!(matches!(err, Error::InvalidState(Invalid::Newer(7))));
    }

    #[test]
    fn render_candidate_refuses_a_list_without_git() {
        let home = tempfile::tempdir().unwrap();

        let err = render_candidate(&user_config(home.path()), &manifest(1, &[])).unwrap_err();

        assert!(matches!(err, Error::InvalidState(Invalid::Missing("git"))));
    }

    #[test]
    fn render_candidate_rejects_an_invalid_package_name() {
        let home = tempfile::tempdir().unwrap();

        let err = render_candidate(
            &user_config(home.path()),
            &manifest(1, &["not a valid ident"]),
        )
        .unwrap_err();

        assert!(matches!(err, Error::InvalidPackage(_)));
    }

    #[tokio::test]
    async fn write_then_switch_restores_previous_content_when_the_switch_fails() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state");
        let home_path = dir.path().join("home.nix");
        std::fs::write(&state_path, "old state").unwrap();
        std::fs::write(&home_path, "old home").unwrap();

        let err = write_then_switch(&state_path, &home_path, "new state", "new home", || {
            std::future::ready(Err(profile::Error::Core(mix_core::Error::Cancelled {
                command: "nix build".to_string(),
            })))
        })
        .await
        .unwrap_err();

        assert!(matches!(err, Error::Activation(_)));
        assert_eq!(std::fs::read_to_string(&state_path).unwrap(), "old state");
        assert_eq!(std::fs::read_to_string(&home_path).unwrap(), "old home");
    }

    #[tokio::test]
    async fn write_then_switch_leaves_nothing_behind_when_there_was_no_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state");
        let home_path = dir.path().join("home.nix");

        write_then_switch(&state_path, &home_path, "new state", "new home", || {
            std::future::ready(Err(profile::Error::Core(mix_core::Error::Cancelled {
                command: "nix build".to_string(),
            })))
        })
        .await
        .unwrap_err();

        assert!(!state_path.exists());
        assert!(!home_path.exists());
    }

    #[tokio::test]
    async fn write_then_switch_keeps_the_new_content_when_the_switch_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state");
        let home_path = dir.path().join("home.nix");
        std::fs::write(&state_path, "old state").unwrap();
        std::fs::write(&home_path, "old home").unwrap();

        write_then_switch(&state_path, &home_path, "new state", "new home", || {
            std::future::ready(Ok("/nix/store/generation".to_string()))
        })
        .await
        .unwrap();

        assert_eq!(std::fs::read_to_string(&state_path).unwrap(), "new state");
        assert_eq!(std::fs::read_to_string(&home_path).unwrap(), "new home");
    }
}
