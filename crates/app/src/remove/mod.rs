use std::sync::Arc;

use mix_core::ActivityReporter;
use mix_core::models::UserConfig;
use mix_core::state::StateManifest;

use crate::profile::BuildPolicy;
use crate::profile::change;
use crate::profile::state::Source;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Change(#[from] change::Error),

    #[error("{} cannot be removed", .0.join(", "))]
    Protected(Vec<String>),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Removed {
    pub removed: Vec<String>,
    pub skipped: Vec<String>,
    #[serde(skip)]
    pub restored: Option<Source>,
}

impl Removed {
    pub fn changed_nothing(&self) -> bool {
        self.removed.is_empty()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("Removed always serializes")
    }
}

pub async fn remove(
    cfg: &UserConfig,
    packages: &[String],
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    activity: Arc<dyn ActivityReporter>,
) -> Result<Removed> {
    let protected = StateManifest::protected(packages);
    if !protected.is_empty() {
        return Err(Error::Protected(
            protected.into_iter().map(str::to_string).collect(),
        ));
    }

    let (state, restored) = change::settled(cfg).await?;
    let partition = state.partition(packages);
    let removed: Vec<String> = partition.installed.iter().map(|p| p.to_string()).collect();
    let skipped: Vec<String> = partition.missing.iter().map(|p| p.to_string()).collect();

    if removed.is_empty() {
        return Ok(Removed {
            removed,
            skipped,
            restored,
        });
    }

    change::apply(
        cfg,
        &state.without(&removed),
        &change::label("Removing", &removed),
        mirror,
        mirror_key,
        &activity,
        BuildPolicy::AllowSource,
    )
    .await?;

    Ok(Removed {
        removed,
        skipped,
        restored,
    })
}

#[cfg(test)]
mod tests {
    use mix_core::NoopActivity;
    use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::privilege::InvokingUser;

    use super::*;

    fn noop() -> Arc<dyn ActivityReporter> {
        Arc::new(NoopActivity)
    }

    fn home_with(packages: &[&str]) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path())).unwrap();
        let manifest = StateManifest {
            version: 1,
            packages: packages.iter().map(|p| p.to_string()).collect(),
        };
        std::fs::write(
            mix_state_dir(home.path()).join(STATE_FILE),
            manifest.render(),
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

    fn state_of(home: &std::path::Path) -> String {
        std::fs::read_to_string(mix_state_dir(home).join(STATE_FILE)).unwrap()
    }

    async fn remove_from(home: &std::path::Path, packages: &[&str]) -> Result<Removed> {
        let packages: Vec<String> = packages.iter().map(|p| p.to_string()).collect();
        remove(&user_config(home), &packages, None, None, noop()).await
    }

    #[tokio::test]
    async fn remove_skips_a_package_that_is_not_installed() {
        let home = home_with(&["git", "ripgrep"]);

        let removed = remove_from(home.path(), &["fd"]).await.unwrap();

        assert!(removed.changed_nothing());
        assert_eq!(removed.skipped, vec!["fd".to_string()]);
    }

    #[tokio::test]
    async fn remove_touches_nothing_when_no_package_is_installed() {
        let home = home_with(&["git", "ripgrep"]);
        let state_before = state_of(home.path());

        remove_from(home.path(), &["fd", "bat"]).await.unwrap();

        assert_eq!(state_of(home.path()), state_before);
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[tokio::test]
    async fn remove_reports_a_repeated_package_once() {
        let home = home_with(&["git"]);

        let removed = remove_from(home.path(), &["fd", "fd"]).await.unwrap();

        assert_eq!(removed.skipped, vec!["fd".to_string()]);
        assert!(removed.removed.is_empty());
    }

    #[tokio::test]
    async fn remove_refuses_a_package_mix_relies_on() {
        let home = home_with(&["git", "ripgrep"]);
        let state_before = state_of(home.path());

        let err = remove_from(home.path(), &["git"]).await.unwrap_err();

        assert!(matches!(&err, Error::Protected(names) if names == &["git".to_string()]));
        assert_eq!(state_of(home.path()), state_before);
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[tokio::test]
    async fn remove_refuses_the_whole_request_when_one_package_is_protected() {
        let home = home_with(&["git", "ripgrep"]);
        let state_before = state_of(home.path());

        let err = remove_from(home.path(), &["ripgrep", "git"])
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Protected(_)));
        assert_eq!(state_of(home.path()), state_before);
    }

    #[tokio::test]
    async fn remove_refuses_a_protected_package_even_when_the_manifest_lacks_it() {
        let home = home_with(&["ripgrep"]);

        let err = remove_from(home.path(), &["git"]).await.unwrap_err();

        assert!(matches!(err, Error::Protected(_)));
    }

    #[test]
    fn the_protected_refusal_names_every_package() {
        let err = Error::Protected(vec!["git".to_string(), "curl".to_string()]);

        assert_eq!(err.to_string(), "git, curl cannot be removed");
    }

    #[test]
    fn the_json_report_names_both_sides_of_the_request() {
        let removed = Removed {
            removed: vec!["ripgrep".to_string(), "fd".to_string()],
            skipped: vec!["bat".to_string()],
            restored: None,
        };

        assert_eq!(
            removed.to_json(),
            r#"{"removed":["ripgrep","fd"],"skipped":["bat"]}"#
        );
    }

    #[test]
    fn the_json_report_keeps_both_keys_when_nothing_was_removed() {
        assert_eq!(
            Removed::default().to_json(),
            r#"{"removed":[],"skipped":[]}"#
        );
    }
}
