use mix_core::change::Refusal;
use mix_events::v1::RemoveRequest;

use crate::Context;
use crate::profile::change;
use crate::request::{Concluded, Root};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Change(#[from] change::Error),

    #[error("{} cannot be removed", .0.join(", "))]
    Protected(Vec<String>),
}

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

pub(crate) async fn remove(ctx: &Context, root: &mut Root, request: &RemoveRequest) -> Concluded {
    if ctx.caller_is_root {
        return root.refuse(Error::Change(change::Error::NotRoot));
    }
    let Some(cfg) = ctx.user.as_ref() else {
        return root.refuse(Error::Change(change::Error::NotBootstrapped));
    };
    let decided =
        match mix_core::change::remove(&request.packages, change::settled(cfg, &ctx.locked)) {
            Ok(decided) => decided,
            Err(refused) => return root.refuse(Error::from(refused)),
        };
    change::run(ctx, root, cfg, change::Verb::Remove, &decided).await
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::identity::InvokingUser;
    use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::state::StateManifest;
    use mix_events::v1::command::Request;
    use mix_events::v1::diagnostic::Detail;
    use mix_events::v1::{Code, RemoveResult, node_finished};

    use super::*;
    use crate::request::ran::{Ran, ran};

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
        crate::Session::new(mix_exec::Scope::root())
            .with_user(Some(user_config(home)))
            .with_journals(home.join("journal"))
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

    async fn remove_from(home: &std::path::Path, packages: &[&str]) -> Ran {
        let packages = packages.iter().map(|p| p.to_string()).collect();
        ran(context(home), Request::Remove(RemoveRequest { packages })).await
    }

    fn removed(ran: &Ran) -> &RemoveResult {
        match ran.result() {
            Some(node_finished::Result::Remove(removed)) => removed,
            other => panic!("expected a remove result, got {other:?}"),
        }
    }

    fn protected(ran: &Ran) -> Vec<String> {
        assert_eq!(ran.code(), Some(Code::ProtectedPackage));
        match ran
            .root()
            .diagnostic
            .as_ref()
            .and_then(|d| d.detail.as_ref())
        {
            Some(Detail::Packages(detail)) => detail.packages.clone(),
            other => panic!("expected the protected packages, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_skips_a_package_that_is_not_installed() {
        let home = home_with(&["git", "ripgrep"]);

        let ran = remove_from(home.path(), &["fd"]).await;

        assert!(removed(&ran).removed.is_empty());
        assert_eq!(removed(&ran).skipped, vec!["fd".to_string()]);
    }

    #[tokio::test]
    async fn remove_touches_nothing_when_no_package_is_installed() {
        let home = home_with(&["git", "ripgrep"]);
        let state_before = state_of(home.path());

        remove_from(home.path(), &["fd", "bat"]).await;

        assert_eq!(state_of(home.path()), state_before);
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[tokio::test]
    async fn remove_reports_a_repeated_package_once() {
        let home = home_with(&["git"]);

        let ran = remove_from(home.path(), &["fd", "fd"]).await;

        assert_eq!(removed(&ran).skipped, vec!["fd".to_string()]);
        assert!(removed(&ran).removed.is_empty());
    }

    #[tokio::test]
    async fn remove_refuses_a_package_mix_relies_on() {
        let home = home_with(&["git", "ripgrep"]);
        let state_before = state_of(home.path());

        let ran = remove_from(home.path(), &["git"]).await;

        assert_eq!(protected(&ran), vec!["git".to_string()]);
        assert_eq!(state_of(home.path()), state_before);
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[tokio::test]
    async fn remove_refuses_the_whole_request_when_one_package_is_protected() {
        let home = home_with(&["git", "ripgrep"]);
        let state_before = state_of(home.path());

        let ran = remove_from(home.path(), &["ripgrep", "git"]).await;

        assert_eq!(protected(&ran), vec!["git".to_string()]);
        assert_eq!(state_of(home.path()), state_before);
    }

    #[tokio::test]
    async fn remove_refuses_a_protected_package_even_when_the_manifest_lacks_it() {
        let home = home_with(&["ripgrep"]);

        let ran = remove_from(home.path(), &["git"]).await;

        assert_eq!(protected(&ran), vec!["git".to_string()]);
    }

    #[test]
    fn the_protected_refusal_names_every_package() {
        let err = Error::Protected(vec!["git".to_string(), "curl".to_string()]);

        assert_eq!(err.to_string(), "git, curl cannot be removed");
    }
}
