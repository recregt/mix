use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use mix_core::models::UserConfig;
use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
use mix_core::state::StateManifest;
use mix_core::{ActivityReporter, CancellationToken};
use tracing::Instrument;

use crate::fs::write_atomic;
use crate::profile::config::{read_state, render_home};
use crate::profile::{self, BuildPolicy};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error(transparent)]
    Activation(#[from] crate::profile::Error),

    #[error(transparent)]
    InvalidPackage(#[from] mix_nixgen::InvalidInput),

    #[error("this command installs into the invoking user's profile, and it was run as root")]
    NotRoot,

    #[error("no managed environment was found for the invoking user")]
    NotBootstrapped,
}

pub type Result<T> = std::result::Result<T, Error>;

/// What a run of `install` actually did, so a caller can report an idempotent no-op as a
/// success rather than a failure.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Installed {
    /// Packages added to the profile by this run, in the order they were requested.
    pub added: Vec<String>,
    /// Packages that were already in the profile and were left alone.
    pub skipped: Vec<String>,
}

impl Installed {
    pub fn changed_nothing(&self) -> bool {
        self.added.is_empty()
    }

    /// The result as one line of JSON, for a script that would rather not read prose.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("Installed always serializes")
    }
}

pub async fn install(
    cfg: &UserConfig,
    packages: &[String],
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    activity: Arc<dyn ActivityReporter>,
    allow_source_builds: bool,
) -> Result<Installed> {
    let state = read_state(&cfg.user.home);
    let partition = state.partition(packages);
    let skipped: Vec<String> = partition.installed.iter().map(|p| p.to_string()).collect();
    let added: Vec<String> = partition.missing.iter().map(|p| p.to_string()).collect();

    // Everything requested is already there: nothing to render, write or build.
    if added.is_empty() {
        return Ok(Installed { added, skipped });
    }

    let (new_state, new_home) = render_candidate(cfg, &state, &added)?;

    let state_dir = mix_state_dir(&cfg.user.home);
    let state_path = state_dir.join(STATE_FILE);
    let home_path = state_dir.join(HOME_NIX);
    let token = CancellationToken::new();

    let label = install_label(&added);
    let span = tracing::info_span!("step", name = label.as_str());

    // Nothing is compiled behind the user's back: unless they asked for it, a package the binary
    // cache cannot serve is refused before anything is built, with the files put back as they
    // were.
    let policy = BuildPolicy::from_allowing_source(allow_source_builds);

    write_then_activate(&state_path, &home_path, &new_state, &new_home, || {
        profile::activate(cfg, mirror, mirror_key, &activity, &token, policy)
    })
    .instrument(span)
    .await?;

    Ok(Installed { added, skipped })
}

/// Names the packages being installed, or counts them once the list stops fitting on a line.
fn install_label(added: &[String]) -> String {
    match added.len() {
        0..=3 => format!("Installing {}", added.join(", ")),
        n => format!("Installing {n} packages"),
    }
}

/// Renders the manifest and the `home.nix` the profile would have once `added` is installed.
///
/// Done before anything is written so an invalid package name fails with the profile untouched.
fn render_candidate(
    cfg: &UserConfig,
    state: &StateManifest,
    added: &[String],
) -> Result<(String, String)> {
    let mut packages = Vec::with_capacity(state.packages.len() + added.len());
    packages.extend_from_slice(&state.packages);
    packages.extend_from_slice(added);

    let home = render_home(&cfg.user, &packages)?;
    let manifest = StateManifest {
        version: state.version,
        packages,
    }
    .render();

    Ok((manifest, home))
}

async fn write_then_activate<F, Fut>(
    state_path: &Path,
    home_path: &Path,
    new_state: &str,
    new_home: &str,
    activate: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = profile::Result<bool>>,
{
    let previous_state = tokio::fs::read_to_string(state_path).await.ok();
    let previous_home = tokio::fs::read_to_string(home_path).await.ok();

    write_atomic(state_path, new_state).await?;
    write_atomic(home_path, new_home).await?;

    if let Err(e) = activate().await {
        if let Some(previous) = previous_state {
            let _ = write_atomic(state_path, &previous).await;
        }
        if let Some(previous) = previous_home {
            let _ = write_atomic(home_path, &previous).await;
        }
        return Err(e.into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use mix_core::NoopActivity;
    use mix_core::privilege::InvokingUser;

    use super::*;

    fn noop() -> Arc<dyn ActivityReporter> {
        Arc::new(NoopActivity)
    }

    fn seeded_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path())).unwrap();
        std::fs::write(
            mix_state_dir(home.path()).join(STATE_FILE),
            StateManifest::seed().render(),
        )
        .unwrap();
        home
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
            home: "home-content".to_string(),
        }
    }

    #[tokio::test]
    async fn install_skips_a_package_that_is_already_present() {
        let home = seeded_home();

        let installed = install(
            &user_config(home.path()),
            &["git".to_string()],
            None,
            None,
            noop(),
            false,
        )
        .await
        .unwrap();

        assert!(installed.changed_nothing());
        assert_eq!(installed.skipped, vec!["git".to_string()]);
    }

    #[tokio::test]
    async fn install_touches_nothing_when_every_package_is_already_present() {
        let home = seeded_home();
        let state_before =
            std::fs::read_to_string(mix_state_dir(home.path()).join(STATE_FILE)).unwrap();

        install(
            &user_config(home.path()),
            &["git".to_string()],
            None,
            None,
            noop(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(mix_state_dir(home.path()).join(STATE_FILE)).unwrap(),
            state_before
        );
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[tokio::test]
    async fn install_reports_a_repeated_package_once() {
        let home = seeded_home();

        let installed = install(
            &user_config(home.path()),
            &["git".to_string(), "git".to_string()],
            None,
            None,
            noop(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(installed.skipped, vec!["git".to_string()]);
        assert!(installed.added.is_empty());
    }

    #[tokio::test]
    async fn install_leaves_nothing_written_for_an_invalid_package_name() {
        let home = seeded_home();

        let err = install(
            &user_config(home.path()),
            &["not a valid ident".to_string()],
            None,
            None,
            noop(),
            false,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, Error::InvalidPackage(_)));
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[test]
    fn the_json_report_names_both_sides_of_the_request() {
        let installed = Installed {
            added: vec!["ripgrep".to_string(), "fd".to_string()],
            skipped: vec!["git".to_string()],
        };

        assert_eq!(
            installed.to_json(),
            r#"{"added":["ripgrep","fd"],"skipped":["git"]}"#
        );
    }

    #[test]
    fn the_json_report_keeps_both_keys_when_nothing_was_installed() {
        assert_eq!(
            Installed::default().to_json(),
            r#"{"added":[],"skipped":[]}"#
        );
    }

    #[test]
    fn install_label_names_a_short_list_of_packages() {
        assert_eq!(
            install_label(&["git".to_string(), "fd".to_string()]),
            "Installing git, fd"
        );
    }

    #[test]
    fn install_label_counts_a_long_list_of_packages() {
        let packages: Vec<String> = (0..12).map(|i| format!("package-{i}")).collect();
        assert_eq!(install_label(&packages), "Installing 12 packages");
    }

    #[test]
    fn render_candidate_keeps_the_installed_packages_and_appends_the_new_ones() {
        let home = tempfile::tempdir().unwrap();
        let state = StateManifest {
            version: 1,
            packages: vec!["git".to_string()],
        };

        let (manifest, home_nix) =
            render_candidate(&user_config(home.path()), &state, &["ripgrep".to_string()]).unwrap();

        assert_eq!(
            StateManifest::parse(&manifest).unwrap().packages,
            vec!["git".to_string(), "ripgrep".to_string()]
        );
        assert!(home_nix.contains("ripgrep"));
        assert!(home_nix.contains("git"));
    }

    #[test]
    fn render_candidate_keeps_the_manifest_version() {
        let home = tempfile::tempdir().unwrap();
        let state = StateManifest {
            version: 7,
            packages: Vec::new(),
        };

        let (manifest, _) =
            render_candidate(&user_config(home.path()), &state, &["fd".to_string()]).unwrap();

        assert_eq!(StateManifest::parse(&manifest).unwrap().version, 7);
    }

    #[test]
    fn render_candidate_rejects_an_invalid_package_name() {
        let home = tempfile::tempdir().unwrap();
        let state = StateManifest::seed();

        let err = render_candidate(
            &user_config(home.path()),
            &state,
            &["not a valid ident".to_string()],
        )
        .unwrap_err();

        assert!(matches!(err, Error::InvalidPackage(_)));
    }

    #[tokio::test]
    async fn write_then_activate_restores_previous_content_when_activation_fails() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state");
        let home_path = dir.path().join("home.nix");
        std::fs::write(&state_path, "old state").unwrap();
        std::fs::write(&home_path, "old home").unwrap();

        let err = write_then_activate(&state_path, &home_path, "new state", "new home", || {
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
    async fn write_then_activate_leaves_nothing_behind_when_there_was_no_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state");
        let home_path = dir.path().join("home.nix");

        write_then_activate(&state_path, &home_path, "new state", "new home", || {
            std::future::ready(Err(profile::Error::Core(mix_core::Error::Cancelled {
                command: "nix build".to_string(),
            })))
        })
        .await
        .unwrap_err();

        assert_eq!(std::fs::read_to_string(&state_path).unwrap(), "new state");
        assert_eq!(std::fs::read_to_string(&home_path).unwrap(), "new home");
    }

    #[tokio::test]
    async fn write_then_activate_keeps_the_new_content_when_activation_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state");
        let home_path = dir.path().join("home.nix");
        std::fs::write(&state_path, "old state").unwrap();
        std::fs::write(&home_path, "old home").unwrap();

        write_then_activate(&state_path, &home_path, "new state", "new home", || {
            std::future::ready(Ok(false))
        })
        .await
        .unwrap();

        assert_eq!(std::fs::read_to_string(&state_path).unwrap(), "new state");
        assert_eq!(std::fs::read_to_string(&home_path).unwrap(), "new home");
    }
}
