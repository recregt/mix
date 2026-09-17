use std::future::Future;
use std::path::Path;

use mix_core::CancellationToken;
use mix_core::models::UserConfig;
use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
use mix_core::state::StateManifest;

use crate::shared::home_manager::{read_state, render_home};
use crate::shared::os::write_file_atomic;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error(transparent)]
    Activation(#[from] crate::bootstrap::Error),

    #[error(transparent)]
    InvalidPackage(#[from] mix_nixgen::InvalidInput),

    #[error("already installed: {}", .0.join(", "))]
    AlreadyInstalled(Vec<String>),

    #[error("'mix install' cannot be run as root.\nRun as a regular user.")]
    NotRoot,

    #[error("not bootstrapped yet.\nRun `mix bootstrap` first.")]
    NotBootstrapped,
}

pub type Result<T> = std::result::Result<T, Error>;

pub async fn install(
    cfg: &UserConfig,
    packages: &[String],
    mirror: Option<&str>,
    mirror_key: Option<&str>,
) -> Result<Vec<String>> {
    let requested = dedupe(packages);
    let state = read_state(&cfg.user.home);

    let already: Vec<String> = requested
        .iter()
        .filter(|p| state.packages.contains(p))
        .cloned()
        .collect();
    if !already.is_empty() {
        return Err(Error::AlreadyInstalled(already));
    }

    let mut candidate_packages = state.packages.clone();
    candidate_packages.extend(requested.iter().cloned());
    let new_home = render_home(&cfg.user, &candidate_packages)?;
    let new_state = StateManifest {
        version: state.version,
        packages: candidate_packages,
    }
    .render();

    let state_dir = mix_state_dir(&cfg.user.home);
    let state_path = state_dir.join(STATE_FILE);
    let home_path = state_dir.join(HOME_NIX);
    let token = CancellationToken::new();

    write_then_activate(&state_path, &home_path, &new_state, &new_home, || {
        crate::bootstrap::activate(cfg, mirror, mirror_key, &token)
    })
    .await?;

    Ok(requested)
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
    Fut: Future<Output = crate::bootstrap::Result<bool>>,
{
    let previous_state = tokio::fs::read_to_string(state_path).await.ok();
    let previous_home = tokio::fs::read_to_string(home_path).await.ok();

    write_file_atomic(state_path, new_state).await?;
    write_file_atomic(home_path, new_home).await?;

    if let Err(e) = activate().await {
        if let Some(previous) = previous_state {
            let _ = write_file_atomic(state_path, &previous).await;
        }
        if let Some(previous) = previous_home {
            let _ = write_file_atomic(home_path, &previous).await;
        }
        return Err(e.into());
    }

    Ok(())
}

fn dedupe(packages: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    packages
        .iter()
        .filter(|p| seen.insert(p.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use mix_core::privilege::InvokingUser;

    use super::*;

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

    #[test]
    fn dedupe_keeps_first_occurrence_order() {
        assert_eq!(
            dedupe(&["git".to_string(), "ripgrep".to_string(), "git".to_string()]),
            vec!["git".to_string(), "ripgrep".to_string()]
        );
    }

    #[tokio::test]
    async fn install_rejects_a_package_already_present() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path())).unwrap();
        std::fs::write(
            mix_state_dir(home.path()).join(STATE_FILE),
            StateManifest::seed().render(),
        )
        .unwrap();

        let err = install(&user_config(home.path()), &["git".to_string()], None, None)
            .await
            .unwrap_err();

        assert!(matches!(err, Error::AlreadyInstalled(pkgs) if pkgs == vec!["git".to_string()]));
    }

    #[tokio::test]
    async fn install_leaves_nothing_written_for_an_invalid_package_name() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path())).unwrap();
        std::fs::write(
            mix_state_dir(home.path()).join(STATE_FILE),
            StateManifest::seed().render(),
        )
        .unwrap();

        let err = install(
            &user_config(home.path()),
            &["not a valid ident".to_string()],
            None,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, Error::InvalidPackage(_)));
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[tokio::test]
    async fn write_then_activate_restores_previous_content_when_activation_fails() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state");
        let home_path = dir.path().join("home.nix");
        std::fs::write(&state_path, "old state").unwrap();
        std::fs::write(&home_path, "old home").unwrap();

        let err = write_then_activate(&state_path, &home_path, "new state", "new home", || {
            std::future::ready(Err(crate::bootstrap::Error::Interrupted))
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
            std::future::ready(Err(crate::bootstrap::Error::Interrupted))
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
