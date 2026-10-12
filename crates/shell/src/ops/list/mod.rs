use mix_events::Ending;
use mix_events::v1::{ListRequest, ListResult, node_finished};

use crate::Context;
use crate::profile::change;
use crate::request::installed::Error;
use crate::request::{Concluded, Root};

pub(crate) async fn list(ctx: &Context, root: &mut Root, _request: &ListRequest) -> Concluded {
    let cfg = match change::profile_user(ctx) {
        Ok(cfg) => cfg,
        Err(error) => return root.refuse(error),
    };
    let listing = match ctx
        .installed
        .listing(&ctx.host, &cfg.user, ctx.mirror(), &ctx.scope)
        .await
    {
        Ok(listing) => listing,
        Err(Error::Profile(error)) => return root.refuse(error),
        Err(Error::Stopped) => {
            return root.refuse(change::Error::Core(mix_core::Error::Cancelled {
                command: "mix list".to_string(),
            }));
        }
    };
    let result = match listing {
        Some(listed) => ListResult {
            packages: listed.packages.to_vec(),
            active: Some((&listed.generation).into()),
        },
        None => ListResult::default(),
    };
    root.conclude(Ending::succeeded().with_result(node_finished::Result::List(result)))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use mix_core::declared::identity::InvokingUser;
    use mix_core::declared::paths::{GENERATION_INPUTS, STATE_FILE};
    use mix_core::declared::state::StateManifest;
    use mix_core::model::World;
    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, InstalledPackage};

    use super::*;
    use crate::request::context::Host;
    use crate::request::installed::{Ask, Gathering, Installed, Source};
    use crate::request::ran::{Ran, ran};

    struct Fixed {
        asked: AtomicUsize,
        mirror: Mutex<Option<Option<String>>>,
        names: Mutex<Vec<String>>,
        failure: bool,
    }

    impl Fixed {
        fn new(failure: bool) -> Arc<Self> {
            Arc::new(Self {
                asked: AtomicUsize::new(0),
                mirror: Mutex::new(None),
                names: Mutex::new(Vec::new()),
                failure,
            })
        }
    }

    impl Source for Fixed {
        fn gather<'a>(&'a self, ask: Ask<'a>) -> Gathering<'a> {
            Box::pin(async move {
                self.asked.fetch_add(1, Ordering::SeqCst);
                *self.mirror.lock().unwrap() = Some(ask.mirror.map(str::to_string));
                *self.names.lock().unwrap() = ask.names.to_vec();
                if self.failure {
                    return Err(mix_core::Error::Command {
                        command: "nix eval".to_string(),
                        detail: "it failed".to_string(),
                    });
                }
                Ok(ask
                    .names
                    .iter()
                    .map(|name| InstalledPackage {
                        name: name.clone(),
                        version: Some("1.0".to_string()),
                        ..InstalledPackage::default()
                    })
                    .collect())
            })
        }
    }

    fn user() -> InvokingUser {
        InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "mix-user".to_string(),
            home: "/home/mix-user".into(),
        }
    }

    fn user_config() -> mix_core::declared::targets::UserConfig {
        mix_core::declared::targets::UserConfig {
            user: user(),
            pins: mix_nixgen::lock::EMBEDDED,
            flake: "flake-content".to_string(),
            lock: mix_nixgen::lock::render(mix_nixgen::lock::EMBEDDED),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    fn world_with(extra: &[&str]) -> Arc<Mutex<World>> {
        let mut manifest = StateManifest::seed();
        manifest
            .packages
            .extend(extra.iter().map(|name| name.to_string()));
        let mut inputs: Vec<Option<Arc<[u8]>>> = vec![None; GENERATION_INPUTS.len()];
        let index = GENERATION_INPUTS
            .iter()
            .position(|(source, _)| *source == STATE_FILE)
            .unwrap();
        inputs[index] = Some(Arc::from(manifest.render().as_bytes()));
        let mut world = World::default();
        let profile = world.profiles.entry(user().uid).or_default();
        profile.generations.push(7);
        profile.built.insert(7, inputs);
        profile.active = Some(7);
        Arc::new(Mutex::new(world))
    }

    fn session(world: &Arc<Mutex<World>>, source: &Arc<Fixed>) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_host(Host::Model(Arc::clone(world)))
            .with_user(Some(user_config()))
            .with_installed(Arc::new(Installed::with_source(
                Arc::clone(source) as Arc<dyn Source>
            )))
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
        let world = world_with(&["hello", "ripgrep"]);
        let source = Fixed::new(false);

        let ran = listed(session(&world, &source)).await;

        let list = answer(&ran);
        let names: Vec<&str> = list
            .packages
            .iter()
            .map(|package| package.name.as_str())
            .collect();
        assert!(names.contains(&"git"));
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"ripgrep"));
        assert_eq!(names.len(), StateManifest::seed().packages.len() + 2);
        let active = list.active.as_ref().expect("an active generation");
        assert_eq!(active.number, 7);
        assert!(active.store_path.starts_with("/nix/store/"));
    }

    #[tokio::test]
    async fn list_with_no_generation_answers_an_empty_list_without_asking_nix() {
        let world = Arc::new(Mutex::new(World::default()));
        let source = Fixed::new(false);

        let ran = listed(session(&world, &source)).await;

        assert_eq!(answer(&ran), &ListResult::default());
        assert_eq!(source.asked.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_second_list_of_a_generation_asks_nix_nothing() {
        let world = world_with(&["hello"]);
        let source = Fixed::new(false);
        let installed = Arc::new(Installed::with_source(
            Arc::clone(&source) as Arc<dyn Source>
        ));
        let again = || {
            crate::Session::new(mix_exec::Scope::root())
                .with_host(Host::Model(Arc::clone(&world)))
                .with_user(Some(user_config()))
                .with_installed(Arc::clone(&installed))
        };

        listed(again()).await;
        let second = listed(again()).await;

        assert!(answer(&second).packages.iter().any(|p| p.name == "hello"));
        assert_eq!(source.asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_gather_ends_the_request_with_the_command_that_failed() {
        let world = world_with(&[]);
        let source = Fixed::new(true);

        let ran = listed(session(&world, &source)).await;

        assert_eq!(ran.code(), Some(Code::CommandFailed));
    }

    #[tokio::test]
    async fn list_hands_the_policy_mirror_to_the_gather() {
        let world = world_with(&[]);
        let source = Fixed::new(false);
        let policy =
            mix_core::declared::policy::Policy::new(Some("http://mirror.internal"), None).unwrap();

        listed(session(&world, &source).with_policy(policy)).await;

        assert_eq!(
            source.mirror.lock().unwrap().clone(),
            Some(Some("http://mirror.internal".to_string()))
        );
    }

    #[tokio::test]
    async fn a_package_list_from_a_newer_mix_asks_for_an_update_instead_of_listing() {
        let world = world_with(&[]);
        let mut manifest = StateManifest::seed();
        manifest.version += 1;
        {
            let mut held = world.lock().unwrap();
            let profile = held.profiles.get_mut(&user().uid).unwrap();
            let inputs = profile.built.get_mut(&7).unwrap();
            let index = GENERATION_INPUTS
                .iter()
                .position(|(source, _)| *source == STATE_FILE)
                .unwrap();
            inputs[index] = Some(Arc::from(manifest.render().as_bytes()));
        }
        let source = Fixed::new(false);

        let ran = listed(session(&world, &source)).await;

        assert_eq!(ran.code(), Some(Code::NewerState));
        assert_eq!(source.asked.load(Ordering::SeqCst), 0);
    }
}
