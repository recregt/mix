use crate::Context;

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

pub async fn install(ctx: &Context, packages: &[String]) -> Result<Installed> {
    let cfg = ctx.user.as_ref().ok_or(change::Error::NotBootstrapped)?;
    let settled = change::settled(cfg);
    let decided = mix_core::change::install(packages, settled.clone())?;
    let restored = (decided.source != Source::File).then_some(decided.source);
    change::run(
        ctx,
        cfg,
        change::Verb::Install,
        packages,
        &decided,
        &mut Vec::new(),
    )
    .await?;

    Ok(Installed {
        added: decided.changed,
        skipped: decided.skipped,
        restored,
    })
}

#[cfg(test)]
mod tests {
    use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::privilege::InvokingUser;
    use mix_core::state::StateManifest;

    use super::*;
    use crate::profile::change::Error;

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

    fn context(home: &std::path::Path) -> Context {
        Context::new(mix_exec::Scope::root()).with_user(Some(user_config(home)))
    }

    fn user_config(home: &std::path::Path) -> mix_core::models::UserConfig {
        mix_core::models::UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: home.to_path_buf(),
            },
            flake: "flake-content".to_string(),
            lock: "lock-content".to_string(),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    #[tokio::test]
    async fn install_skips_a_package_that_is_already_present() {
        let home = seeded_home();

        let installed = install(&context(home.path()), &["git".to_string()])
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

        install(&context(home.path()), &["git".to_string()])
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
            &context(home.path()),
            &["git".to_string(), "git".to_string()],
        )
        .await
        .unwrap();

        assert_eq!(installed.skipped, vec!["git".to_string()]);
        assert!(installed.added.is_empty());
    }

    #[tokio::test]
    async fn install_leaves_nothing_written_for_an_invalid_package_name() {
        let home = seeded_home();

        let err = install(&context(home.path()), &["not a valid ident".to_string()])
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
}
