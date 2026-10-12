use mix_events::Ending;
use mix_events::v1::{ListRequest, ListResult, node_finished};

use crate::Context;
use crate::profile::change;
use crate::request::{Concluded, Root};

pub(crate) async fn list(ctx: &Context, root: &mut Root, _request: &ListRequest) -> Concluded {
    let cfg = match change::profile_user(ctx) {
        Ok(cfg) => cfg,
        Err(error) => return root.refuse(error),
    };
    let mirror = ctx.policy.mirror().map(|mirror| mirror.url().to_string());
    let listing = match ctx
        .installed
        .listing(&cfg.user, mirror.as_deref(), &ctx.scope)
        .await
    {
        Ok(listing) => listing,
        Err(error) => return root.refuse(error),
    };
    let result = match listing {
        Some(listed) => ListResult {
            generation: Some(listed.generation.display().to_string()),
            packages: listed.packages.to_vec(),
        },
        None => ListResult::default(),
    };
    root.conclude(Ending::succeeded().with_result(node_finished::Result::List(result)))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mix_core::declared::identity::InvokingUser;
    use mix_core::declared::paths::{HOME_MANAGER_PROFILE_NAME, nix_profiles_dir};
    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, InstalledPackage};

    use super::*;
    use crate::request::installed::{Ask, Gathering, Installed, Source};
    use crate::request::ran::{Ran, ran};

    struct Fixed {
        packages: Vec<InstalledPackage>,
        asked: AtomicUsize,
        mirror: std::sync::Mutex<Option<Option<String>>>,
        failure: bool,
    }

    impl Fixed {
        fn of(names: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                packages: names
                    .iter()
                    .map(|name| InstalledPackage {
                        name: name.to_string(),
                        version: Some("1.0".to_string()),
                        ..InstalledPackage::default()
                    })
                    .collect(),
                asked: AtomicUsize::new(0),
                mirror: std::sync::Mutex::new(None),
                failure: false,
            })
        }
    }

    impl Source for Fixed {
        fn gather<'a>(&'a self, ask: Ask<'a>) -> Gathering<'a> {
            Box::pin(async move {
                self.asked.fetch_add(1, Ordering::SeqCst);
                *self.mirror.lock().unwrap() = Some(ask.mirror.map(str::to_string));
                if self.failure {
                    return Err(mix_core::Error::Command {
                        command: "nix eval".to_string(),
                        detail: "it failed".to_string(),
                    });
                }
                Ok(self.packages.clone())
            })
        }
    }

    fn user_config(home: &Path) -> mix_core::declared::targets::UserConfig {
        mix_core::declared::targets::UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: home.to_path_buf(),
            },
            pins: mix_nixgen::lock::EMBEDDED,
            flake: "flake-content".to_string(),
            lock: mix_nixgen::lock::render(mix_nixgen::lock::EMBEDDED),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    fn activate(home: &Path) {
        let generation = home.join("store/generation");
        std::fs::create_dir_all(&generation).unwrap();
        let profiles = nix_profiles_dir(home);
        std::fs::create_dir_all(&profiles).unwrap();
        std::os::unix::fs::symlink(generation, profiles.join(HOME_MANAGER_PROFILE_NAME)).unwrap();
    }

    fn store(home: &Path) -> PathBuf {
        std::fs::create_dir_all(home.join("store")).unwrap();
        std::fs::canonicalize(home.join("store")).unwrap()
    }

    fn session(home: &Path, source: &Arc<Fixed>) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_user(Some(user_config(home)))
            .with_installed(Arc::new(
                Installed::with_source(Arc::clone(source) as Arc<dyn Source>).in_store(store(home)),
            ))
    }

    async fn listed(session: crate::Session) -> Ran {
        ran(session, Request::List(ListRequest {})).await
    }

    fn answer(ran: &Ran) -> &ListResult {
        match ran.result() {
            Some(node_finished::Result::List(list)) => list,
            other => panic!("expected a list result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_needs_a_bootstrapped_user() {
        let ran = listed(crate::Session::new(mix_exec::Scope::root())).await;

        assert_eq!(ran.code(), Some(Code::NotBootstrapped));
    }

    #[tokio::test]
    async fn root_cannot_list_a_profile() {
        let session =
            crate::Session::new(mix_exec::Scope::root()).with_caller(crate::Caller::Account {
                peer_is_root: true,
                account: None,
            });

        let ran = listed(session).await;

        assert_eq!(ran.code(), Some(Code::RootNotAllowed));
    }

    #[tokio::test]
    async fn list_names_what_the_active_generation_holds() {
        let home = tempfile::tempdir().unwrap();
        activate(home.path());
        let source = Fixed::of(&["git", "ripgrep"]);

        let ran = listed(session(home.path(), &source)).await;

        let list = answer(&ran);
        assert_eq!(
            list.packages
                .iter()
                .map(|package| package.name.as_str())
                .collect::<Vec<_>>(),
            ["git", "ripgrep"]
        );
        assert_eq!(
            list.generation.as_deref(),
            Some(
                std::fs::canonicalize(home.path().join("store/generation"))
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
        );
    }

    #[tokio::test]
    async fn list_with_no_generation_answers_an_empty_list_without_asking_nix() {
        let home = tempfile::tempdir().unwrap();
        let source = Fixed::of(&["git"]);

        let ran = listed(session(home.path(), &source)).await;

        assert_eq!(answer(&ran), &ListResult::default());
        assert_eq!(source.asked.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_second_list_of_a_generation_asks_nix_nothing() {
        let home = tempfile::tempdir().unwrap();
        activate(home.path());
        let source = Fixed::of(&["git"]);
        let installed = Arc::new(
            Installed::with_source(Arc::clone(&source) as Arc<dyn Source>)
                .in_store(store(home.path())),
        );
        let again = || {
            crate::Session::new(mix_exec::Scope::root())
                .with_user(Some(user_config(home.path())))
                .with_installed(Arc::clone(&installed))
        };

        listed(again()).await;
        let second = listed(again()).await;

        assert_eq!(answer(&second).packages.len(), 1);
        assert_eq!(source.asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_gather_ends_the_request_with_the_command_that_failed() {
        let home = tempfile::tempdir().unwrap();
        activate(home.path());
        let source = Arc::new(Fixed {
            failure: true,
            ..Arc::try_unwrap(Fixed::of(&[])).ok().unwrap()
        });

        let ran = listed(session(home.path(), &source)).await;

        assert_eq!(ran.code(), Some(Code::CommandFailed));
    }

    #[tokio::test]
    async fn list_hands_the_policy_mirror_to_the_gather() {
        let home = tempfile::tempdir().unwrap();
        activate(home.path());
        let source = Fixed::of(&[]);
        let policy =
            mix_core::declared::policy::Policy::new(Some("http://mirror.internal"), None).unwrap();

        listed(session(home.path(), &source).with_policy(policy)).await;

        assert_eq!(
            source.mirror.lock().unwrap().clone(),
            Some(Some("http://mirror.internal".to_string()))
        );
    }
}
