use std::sync::Arc;

use mix_core::ActivityReporter;
use mix_core::models::UserConfig;
use mix_core::state::StateManifest;

use crate::profile::BuildPolicy;
use crate::profile::change::{self, Result};
use crate::profile::state::Source;

/// What a run of `install` actually did, so a caller can report an idempotent no-op as a
/// success rather than a failure.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Installed {
    /// Packages added to the profile by this run, in the order they were requested.
    pub added: Vec<String>,
    /// Packages that were already in the profile and were left alone.
    pub skipped: Vec<String>,
    #[serde(skip)]
    pub restored: Option<Source>,
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
    let (state, restored) = change::settled(cfg).await?;
    let partition = state.partition(packages);
    let skipped: Vec<String> = partition.installed.iter().map(|p| p.to_string()).collect();
    let added: Vec<String> = partition.missing.iter().map(|p| p.to_string()).collect();

    // Everything requested is already there: nothing to render, write or build.
    if added.is_empty() {
        return Ok(Installed {
            added,
            skipped,
            restored,
        });
    }

    let label = change::label("Installing", &added);

    // Nothing is compiled behind the user's back: unless they asked for it, a package the binary
    // cache cannot serve is refused before anything is built, with the files put back as they
    // were.
    let policy = BuildPolicy::from_allowing_source(allow_source_builds);

    change::apply(
        cfg,
        &with_added(&state, &added),
        &label,
        mirror,
        mirror_key,
        &activity,
        policy,
    )
    .await?;

    Ok(Installed {
        added,
        skipped,
        restored,
    })
}

fn with_added(state: &StateManifest, added: &[String]) -> StateManifest {
    let mut packages = Vec::with_capacity(state.packages.len() + added.len());
    packages.extend_from_slice(&state.packages);
    packages.extend_from_slice(added);

    StateManifest {
        version: state.version,
        packages,
    }
}

#[cfg(test)]
mod tests {
    use mix_core::NoopActivity;
    use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::privilege::InvokingUser;

    use super::*;
    use crate::profile::change::Error;

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
            restored_state: None,
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
            restored: None,
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
    fn with_added_keeps_the_installed_packages_and_appends_the_new_ones() {
        let state = StateManifest {
            version: 1,
            packages: vec!["git".to_string()],
        };

        let candidate = with_added(&state, &["ripgrep".to_string()]);

        assert_eq!(
            candidate.packages,
            vec!["git".to_string(), "ripgrep".to_string()]
        );
    }

    #[test]
    fn with_added_keeps_the_manifest_version() {
        let state = StateManifest {
            version: 7,
            packages: Vec::new(),
        };

        assert_eq!(with_added(&state, &["fd".to_string()]).version, 7);
    }
}
