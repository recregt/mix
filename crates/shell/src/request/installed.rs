use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use mix_core::declared::identity::{InvokingUser, MIX_USERS_GROUP};
use mix_core::declared::paths::GENERATION_STATE_FILE;
use mix_core::{Error as CoreError, Result as CoreResult};
use mix_events::v1::InstalledPackage;
use mix_exec::Scope;
use mix_home::Active;

use crate::profile::change;
use crate::profile::state::{Invalid, validate};
use crate::request::context::Host;

pub struct Ask<'a> {
    pub user: &'a InvokingUser,
    pub generation: &'a Active,
    pub names: &'a [String],
    pub mirror: Option<&'a str>,
    pub scope: &'a Scope,
}

pub type Gathering<'a> =
    Pin<Box<dyn Future<Output = CoreResult<Vec<InstalledPackage>>> + Send + 'a>>;

pub trait Source: Send + Sync {
    fn gather<'a>(&'a self, ask: Ask<'a>) -> Gathering<'a>;
}

pub struct Nix;

impl Source for Nix {
    fn gather<'a>(&'a self, ask: Ask<'a>) -> Gathering<'a> {
        Box::pin(async move {
            crate::effect::installed::gather(
                ask.user,
                ask.generation,
                ask.names,
                ask.mirror,
                ask.scope,
            )
            .await
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Profile(#[from] change::Error),

    #[error("the listing was stopped while it waited for another")]
    Stopped,
}

#[derive(Clone)]
pub struct Listed {
    pub generation: Active,
    pub packages: Arc<Vec<InstalledPackage>>,
}

type Slot = Arc<tokio::sync::Mutex<Option<Listed>>>;

pub type Paused = tokio::sync::OwnedRwLockWriteGuard<()>;

pub struct Installed {
    source: Arc<dyn Source>,
    users: Mutex<HashMap<u32, Slot>>,
    running: Arc<tokio::sync::RwLock<()>>,
}

fn requested(generation: &Active) -> change::Result<Vec<String>> {
    let path = generation.path.join(GENERATION_STATE_FILE);
    let unreadable = |source: std::io::Error| {
        change::Error::Core(CoreError::Io {
            path: path.clone(),
            source,
        })
    };
    let Some(raw) = generation.list.as_deref() else {
        return Err(unreadable(std::io::ErrorKind::NotFound.into()));
    };
    match validate(raw) {
        Ok(manifest) => Ok(manifest.packages),
        Err(Invalid::Newer(version)) => Err(change::Error::NewerState(version)),
        Err(invalid) => Err(unreadable(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            invalid,
        ))),
    }
}

impl Installed {
    pub fn nix() -> Self {
        Self::with_source(Arc::new(Nix))
    }

    pub fn with_source(source: Arc<dyn Source>) -> Self {
        Self {
            source,
            users: Mutex::new(HashMap::new()),
            running: Arc::new(tokio::sync::RwLock::new(())),
        }
    }

    pub async fn pause(&self, scope: &Scope) -> std::result::Result<Paused, Error> {
        scope
            .guard(Arc::clone(&self.running).write_owned())
            .await
            .map_err(|_| Error::Stopped)
    }

    fn slot(&self, uid: u32) -> Slot {
        Arc::clone(
            self.users
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(uid)
                .or_default(),
        )
    }

    pub async fn listing(
        &self,
        host: &Host,
        user: &InvokingUser,
        mirror: Option<&str>,
        scope: &Scope,
    ) -> std::result::Result<Option<Listed>, Error> {
        let _running = scope
            .guard(self.running.read())
            .await
            .map_err(|_| Error::Stopped)?;
        let slot = self.slot(user.uid);
        let mut held = scope.guard(slot.lock()).await.map_err(|_| Error::Stopped)?;
        let found = host
            .active_generation(user, scope)
            .await
            .map_err(|failure| {
                change::Error::Core(crate::effect::core_error(failure, &user.home))
            })?;
        let Some(generation) = found else {
            return Ok(None);
        };
        if let Some(listed) = held.as_mut()
            && listed.generation.path == generation.path
        {
            listed.generation = generation;
            return Ok(Some(listed.clone()));
        }
        let names = requested(&generation)?;
        let packages = self
            .source
            .gather(Ask {
                user,
                generation: &generation,
                names: &names,
                mirror,
                scope,
            })
            .await
            .map_err(change::Error::Core)?;
        let listed = Listed {
            generation,
            packages: Arc::new(packages),
        };
        *held = Some(listed.clone());
        Ok(Some(listed))
    }

    pub fn warm(self: &Arc<Self>, host: Host, user: InvokingUser, mirror: Option<String>) {
        if !matches!(host, Host::Machine) {
            return;
        }
        let installed = Arc::clone(self);
        tokio::spawn(async move {
            let _ = installed
                .listing(&host, &user, mirror.as_deref(), &Scope::root())
                .await;
        });
    }

    pub fn warm_enrolled(self: &Arc<Self>, mirror: Option<String>) {
        let installed = Arc::clone(self);
        tokio::spawn(async move {
            let scope = Scope::root();
            for user in crate::effect::accounts::group_members(MIX_USERS_GROUP) {
                let _ = installed
                    .listing(&Host::Machine, &user, mirror.as_deref(), &scope)
                    .await;
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mix_core::declared::paths::{GENERATION_INPUTS, STATE_FILE};
    use mix_core::declared::state::StateManifest;
    use mix_core::model::World;

    use super::*;

    #[derive(Default)]
    struct Counted {
        gathered: AtomicUsize,
        fail: Mutex<usize>,
        gate: Option<Arc<tokio::sync::Notify>>,
        asked: Mutex<Vec<Vec<String>>>,
    }

    impl Source for Counted {
        fn gather<'a>(&'a self, ask: Ask<'a>) -> Gathering<'a> {
            Box::pin(async move {
                self.gathered.fetch_add(1, Ordering::SeqCst);
                self.asked.lock().unwrap().push(ask.names.to_vec());
                if let Some(gate) = &self.gate {
                    gate.notified().await;
                }
                let mut fail = self.fail.lock().unwrap();
                if *fail > 0 {
                    *fail -= 1;
                    return Err(CoreError::Command {
                        command: "nix eval".to_string(),
                        detail: "no".to_string(),
                    });
                }
                Ok(vec![InstalledPackage {
                    name: ask.generation.number.to_string(),
                    ..InstalledPackage::default()
                }])
            })
        }
    }

    fn user(uid: u32) -> InvokingUser {
        InvokingUser {
            uid,
            gid: uid,
            name: format!("user{uid}"),
            home: PathBuf::from(format!("/home/user{uid}")),
        }
    }

    fn list(extra: &[&str]) -> String {
        let mut manifest = StateManifest::seed();
        manifest
            .packages
            .extend(extra.iter().map(|name| name.to_string()));
        manifest.render()
    }

    fn activate(world: &Mutex<World>, user: &InvokingUser, number: u64, list: &str) {
        let mut inputs: Vec<Option<Arc<[u8]>>> = vec![None; GENERATION_INPUTS.len()];
        let index = GENERATION_INPUTS
            .iter()
            .position(|(source, _)| *source == STATE_FILE)
            .unwrap();
        inputs[index] = Some(Arc::from(list.as_bytes()));
        let mut world = world.lock().unwrap();
        let profile = world.profiles.entry(user.uid).or_default();
        profile.generations.push(number);
        profile.built.insert(number, inputs);
        profile.active = Some(number);
    }

    fn hosted() -> (Arc<Mutex<World>>, Host) {
        let world = Arc::new(Mutex::new(World::default()));
        let host = Host::Model(Arc::clone(&world));
        (world, host)
    }

    #[tokio::test]
    async fn a_user_with_no_generation_has_nothing_to_list_and_nothing_is_asked() {
        let (_, host) = hosted();
        let source = Arc::new(Counted::default());

        let listing = Installed::with_source(Arc::clone(&source) as Arc<dyn Source>)
            .listing(&host, &user(1000), None, &Scope::root())
            .await
            .unwrap();

        assert!(listing.is_none());
        assert_eq!(source.gathered.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_source_is_asked_for_the_names_the_generation_was_built_from() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&["hello", "ripgrep"]));
        let source = Arc::new(Counted::default());

        Installed::with_source(Arc::clone(&source) as Arc<dyn Source>)
            .listing(&host, &user(1000), None, &Scope::root())
            .await
            .unwrap();

        let asked = source.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].iter().any(|name| name == "hello"));
        assert!(asked[0].iter().any(|name| name == "ripgrep"));
    }

    #[tokio::test]
    async fn the_second_listing_of_a_generation_is_answered_from_memory() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        let source = Arc::new(Counted::default());
        let installed = Installed::with_source(Arc::clone(&source) as Arc<dyn Source>);
        let scope = Scope::root();

        let first = installed
            .listing(&host, &user(1000), None, &scope)
            .await
            .unwrap()
            .unwrap();
        let second = installed
            .listing(&host, &user(1000), None, &scope)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(source.gathered.load(Ordering::SeqCst), 1);
        assert_eq!(first.generation.number, 1);
        assert!(Arc::ptr_eq(&first.packages, &second.packages));
    }

    #[tokio::test]
    async fn a_new_generation_is_gathered_again_and_the_old_answer_is_dropped() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        let source = Arc::new(Counted::default());
        let installed = Installed::with_source(Arc::clone(&source) as Arc<dyn Source>);
        let scope = Scope::root();
        installed
            .listing(&host, &user(1000), None, &scope)
            .await
            .unwrap();

        activate(&world, &user(1000), 2, &list(&["hello"]));
        let listed = installed
            .listing(&host, &user(1000), None, &scope)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(source.gathered.load(Ordering::SeqCst), 2);
        assert_eq!(listed.generation.number, 2);
        assert_eq!(listed.packages[0].name, "2");
    }

    #[tokio::test]
    async fn a_later_generation_built_from_the_same_files_is_listed_under_its_own_number() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 2, &list(&[]));
        let source = Arc::new(Counted::default());
        let installed = Installed::with_source(Arc::clone(&source) as Arc<dyn Source>);
        let scope = Scope::root();
        let first = installed
            .listing(&host, &user(1000), None, &scope)
            .await
            .unwrap()
            .unwrap();

        activate(&world, &user(1000), 4, &list(&[]));
        let again = installed
            .listing(&host, &user(1000), None, &scope)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(first.generation.path, again.generation.path);
        assert_eq!(source.gathered.load(Ordering::SeqCst), 1);
        assert_eq!(again.generation.number, 4);
        assert!(Arc::ptr_eq(&first.packages, &again.packages));
    }

    #[tokio::test]
    async fn a_failed_gather_is_not_remembered() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        let source = Arc::new(Counted {
            fail: Mutex::new(1),
            ..Counted::default()
        });
        let installed = Installed::with_source(Arc::clone(&source) as Arc<dyn Source>);
        let scope = Scope::root();

        assert!(
            installed
                .listing(&host, &user(1000), None, &scope)
                .await
                .is_err()
        );
        let retried = installed
            .listing(&host, &user(1000), None, &scope)
            .await
            .unwrap();

        assert!(retried.is_some());
        assert_eq!(source.gathered.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn listings_that_arrive_together_share_one_gather() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        let gate = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(Counted {
            gate: Some(Arc::clone(&gate)),
            ..Counted::default()
        });
        let installed = Arc::new(Installed::with_source(
            Arc::clone(&source) as Arc<dyn Source>
        ));
        let listings: Vec<_> = (0..4)
            .map(|_| {
                let installed = Arc::clone(&installed);
                let host = host.clone();
                tokio::spawn(async move {
                    installed
                        .listing(&host, &user(1000), None, &Scope::root())
                        .await
                })
            })
            .collect();
        while source.gathered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        gate.notify_one();

        for listing in listings {
            assert!(listing.await.unwrap().unwrap().is_some());
        }
        assert_eq!(source.gathered.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_listing_stopped_while_it_waits_for_another_ends_stopped() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        let gate = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(Counted {
            gate: Some(Arc::clone(&gate)),
            ..Counted::default()
        });
        let installed = Arc::new(Installed::with_source(
            Arc::clone(&source) as Arc<dyn Source>
        ));
        let first = {
            let installed = Arc::clone(&installed);
            let host = host.clone();
            tokio::spawn(async move {
                installed
                    .listing(&host, &user(1000), None, &Scope::root())
                    .await
            })
        };
        while source.gathered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let scope = Scope::root();
        scope.cancel(mix_exec::Reason::Interrupted);

        let stopped = installed.listing(&host, &user(1000), None, &scope).await;

        assert!(matches!(stopped, Err(Error::Stopped)));
        gate.notify_one();
        assert!(first.await.unwrap().unwrap().is_some());
    }

    #[tokio::test]
    async fn a_pause_waits_for_a_running_listing_and_holds_back_the_next_one() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        activate(&world, &user(1001), 1, &list(&[]));
        let gate = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(Counted {
            gate: Some(Arc::clone(&gate)),
            ..Counted::default()
        });
        let installed = Arc::new(Installed::with_source(
            Arc::clone(&source) as Arc<dyn Source>
        ));
        let listing = |uid: u32| {
            let installed = Arc::clone(&installed);
            let host = host.clone();
            tokio::spawn(async move {
                installed
                    .listing(&host, &user(uid), None, &Scope::root())
                    .await
            })
        };
        let running = listing(1000);
        while source.gathered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let pausing = {
            let installed = Arc::clone(&installed);
            tokio::spawn(async move { installed.pause(&Scope::root()).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            !pausing.is_finished(),
            "a pause waits for the running listing"
        );

        gate.notify_one();
        assert!(running.await.unwrap().unwrap().is_some());
        let paused = pausing.await.unwrap().unwrap();
        let held_back = listing(1001);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(source.gathered.load(Ordering::SeqCst), 1);

        drop(paused);
        gate.notify_one();
        assert!(held_back.await.unwrap().unwrap().is_some());
        assert_eq!(source.gathered.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_pause_stopped_while_a_listing_runs_ends_stopped() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        let gate = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(Counted {
            gate: Some(Arc::clone(&gate)),
            ..Counted::default()
        });
        let installed = Arc::new(Installed::with_source(
            Arc::clone(&source) as Arc<dyn Source>
        ));
        let running = {
            let installed = Arc::clone(&installed);
            let host = host.clone();
            tokio::spawn(async move {
                installed
                    .listing(&host, &user(1000), None, &Scope::root())
                    .await
            })
        };
        while source.gathered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let scope = Scope::root();
        scope.cancel(mix_exec::Reason::Interrupted);

        assert!(matches!(installed.pause(&scope).await, Err(Error::Stopped)));
        gate.notify_one();
        assert!(running.await.unwrap().unwrap().is_some());
    }

    #[tokio::test]
    async fn each_user_has_a_record_of_their_own() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        activate(&world, &user(1001), 1, &list(&[]));
        let source = Arc::new(Counted::default());
        let installed = Installed::with_source(Arc::clone(&source) as Arc<dyn Source>);
        let scope = Scope::root();

        installed
            .listing(&host, &user(1000), None, &scope)
            .await
            .unwrap();
        installed
            .listing(&host, &user(1001), None, &scope)
            .await
            .unwrap();

        assert_eq!(source.gathered.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_generation_whose_store_path_is_gone_has_nothing_to_list() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        world
            .lock()
            .unwrap()
            .profiles
            .get_mut(&1000)
            .unwrap()
            .dangling
            .push(1);
        let source = Arc::new(Counted::default());

        let listing = Installed::with_source(Arc::clone(&source) as Arc<dyn Source>)
            .listing(&host, &user(1000), None, &Scope::root())
            .await
            .unwrap();

        assert!(listing.is_none());
        assert_eq!(source.gathered.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_listing_that_waited_answers_for_the_generation_active_when_it_ran() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        let gate = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(Counted {
            gate: Some(Arc::clone(&gate)),
            ..Counted::default()
        });
        let installed = Arc::new(Installed::with_source(
            Arc::clone(&source) as Arc<dyn Source>
        ));
        let spawn = || {
            let installed = Arc::clone(&installed);
            let host = host.clone();
            tokio::spawn(async move {
                installed
                    .listing(&host, &user(1000), None, &Scope::root())
                    .await
            })
        };
        let first = spawn();
        while source.gathered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let second = spawn();
        tokio::task::yield_now().await;
        activate(&world, &user(1000), 2, &list(&["hello"]));
        gate.notify_one();
        assert_eq!(first.await.unwrap().unwrap().unwrap().generation.number, 1);
        gate.notify_one();

        let waited = second.await.unwrap().unwrap().unwrap();
        let again = installed
            .listing(&host, &user(1000), None, &Scope::root())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(waited.generation.number, 2);
        assert_eq!(again.generation.number, 2);
        assert_eq!(source.gathered.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_generation_without_its_package_list_is_an_io_error_naming_the_file() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, &list(&[]));
        world
            .lock()
            .unwrap()
            .profiles
            .get_mut(&1000)
            .unwrap()
            .built
            .insert(1, vec![None; GENERATION_INPUTS.len()]);

        let failed = Installed::with_source(Arc::new(Counted::default()))
            .listing(&host, &user(1000), None, &Scope::root())
            .await;

        match failed {
            Err(Error::Profile(change::Error::Core(CoreError::Io { path, source }))) => {
                assert!(path.ends_with(GENERATION_STATE_FILE));
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected an I/O error, got {:?}", other.err()),
        }
    }

    #[tokio::test]
    async fn a_package_list_that_is_not_json_is_invalid_data() {
        let (world, host) = hosted();
        activate(&world, &user(1000), 1, "{broken");

        let failed = Installed::with_source(Arc::new(Counted::default()))
            .listing(&host, &user(1000), None, &Scope::root())
            .await;

        match failed {
            Err(Error::Profile(change::Error::Core(CoreError::Io { source, .. }))) => {
                assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
            }
            other => panic!("expected an I/O error, got {:?}", other.err()),
        }
    }

    #[tokio::test]
    async fn a_package_list_that_would_not_pass_the_checks_install_runs_is_not_listed() {
        let (world, host) = hosted();
        let mut manifest = StateManifest::seed();
        manifest.packages.retain(|name| name != "git");
        activate(&world, &user(1000), 1, &manifest.render());
        let source = Arc::new(Counted::default());

        let failed = Installed::with_source(Arc::clone(&source) as Arc<dyn Source>)
            .listing(&host, &user(1000), None, &Scope::root())
            .await;

        assert!(matches!(
            failed,
            Err(Error::Profile(change::Error::Core(CoreError::Io { .. })))
        ));
        assert_eq!(source.gathered.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_package_list_from_a_newer_mix_is_told_apart_from_a_broken_one() {
        let (world, host) = hosted();
        let mut manifest = StateManifest::seed();
        manifest.version += 1;
        activate(&world, &user(1000), 1, &manifest.render());

        let failed = Installed::with_source(Arc::new(Counted::default()))
            .listing(&host, &user(1000), None, &Scope::root())
            .await;

        assert!(matches!(
            failed,
            Err(Error::Profile(change::Error::NewerState(version))) if version == manifest.version
        ));
    }
}
