use mix_core::change::Refusal;

use crate::Context;
use crate::request::{Concluded, Root};

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

impl From<mix_core::Error> for Error {
    fn from(error: mix_core::Error) -> Self {
        Error::Change(change::Error::Core(error))
    }
}

impl From<Refusal> for Error {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Protected(packages) => Error::Protected(packages),
            Refusal::Newer(newer) => Error::Change(newer.into()),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub removed: Vec<String>,
    pub skipped: Vec<String>,
    pub restored: Option<Source>,
}

impl Removed {
    pub fn changed_nothing(&self) -> bool {
        self.removed.is_empty()
    }
}

pub(crate) async fn remove(
    ctx: &Context,
    root: &mut Root,
    packages: &[String],
) -> Concluded<Result<Removed>> {
    if ctx.caller_is_root {
        return root.refuse(Error::Change(change::Error::NotRoot));
    }
    let Some(cfg) = ctx.user.as_ref() else {
        return root.refuse(Error::Change(change::Error::NotBootstrapped));
    };
    let decided = match mix_core::change::remove(packages, change::settled(cfg, &ctx.locked)) {
        Ok(decided) => decided,
        Err(refused) => return root.refuse(Error::from(refused)),
    };
    let restored = (decided.source != Source::File).then_some(decided.source);
    change::run(
        ctx,
        root,
        cfg,
        change::Verb::Remove,
        &decided,
        &mut Vec::new(),
    )
    .await
    .map(|ran| {
        ran.map(|()| Removed {
            removed: decided.changed,
            skipped: decided.skipped,
            restored,
        })
        .map_err(Error::from)
    })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::identity::InvokingUser;
    use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::state::StateManifest;

    use super::*;

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

    fn context(home: &std::path::Path) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root()).with_user(Some(user_config(home)))
    }

    fn user_config(home: &std::path::Path) -> mix_core::targets::UserConfig {
        mix_core::targets::UserConfig {
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

    fn state_of(home: &std::path::Path) -> String {
        std::fs::read_to_string(mix_state_dir(home).join(STATE_FILE)).unwrap()
    }

    async fn remove_from(home: &std::path::Path, packages: &[&str]) -> Result<Removed> {
        let packages: Vec<String> = packages.iter().map(|p| p.to_string()).collect();
        crate::request::remove(&context(home), &packages).await
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
}
