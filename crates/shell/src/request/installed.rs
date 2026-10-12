use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use mix_core::declared::identity::{InvokingUser, MIX_USERS_GROUP};
use mix_core::{Error, Result};
use mix_events::v1::InstalledPackage;
use mix_exec::Scope;

pub struct Ask<'a> {
    pub user: &'a InvokingUser,
    pub generation: &'a Path,
    pub mirror: Option<&'a str>,
    pub scope: &'a Scope,
}

pub type Gathering<'a> = Pin<Box<dyn Future<Output = Result<Vec<InstalledPackage>>> + Send + 'a>>;

pub trait Source: Send + Sync {
    fn gather<'a>(&'a self, ask: Ask<'a>) -> Gathering<'a>;
}

pub struct Nix;

impl Source for Nix {
    fn gather<'a>(&'a self, ask: Ask<'a>) -> Gathering<'a> {
        Box::pin(async move {
            crate::effect::installed::gather(ask.user, ask.generation, ask.mirror, ask.scope).await
        })
    }
}

#[derive(Clone)]
pub struct Listed {
    pub generation: PathBuf,
    pub packages: Arc<Vec<InstalledPackage>>,
}

type Slot = Arc<tokio::sync::Mutex<Option<Listed>>>;

pub struct Installed {
    source: Arc<dyn Source>,
    store: PathBuf,
    users: Mutex<HashMap<u32, Slot>>,
}

impl Installed {
    pub fn nix() -> Self {
        Self::with_source(Arc::new(Nix))
    }

    pub fn with_source(source: Arc<dyn Source>) -> Self {
        Self {
            source,
            store: PathBuf::from(mix_core::declared::paths::NIX_STORE),
            users: Mutex::new(HashMap::new()),
        }
    }

    pub fn in_store(mut self, store: impl Into<PathBuf>) -> Self {
        self.store = store.into();
        self
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
        user: &InvokingUser,
        mirror: Option<&str>,
        scope: &Scope,
    ) -> Result<Option<Listed>> {
        let link = crate::effect::generations::profile_link(user);
        let Some(generation) = crate::effect::generations::store_generation(&link, &self.store)
        else {
            return Ok(None);
        };
        let slot = self.slot(user.uid);
        let mut held = scope
            .guard(slot.lock())
            .await
            .map_err(|_| Error::Cancelled {
                command: "mix list".to_string(),
            })?;
        if let Some(listed) = held.as_ref()
            && listed.generation == generation
        {
            return Ok(Some(listed.clone()));
        }
        let packages = self
            .source
            .gather(Ask {
                user,
                generation: &generation,
                mirror,
                scope,
            })
            .await?;
        let listed = Listed {
            generation,
            packages: Arc::new(packages),
        };
        *held = Some(listed.clone());
        Ok(Some(listed))
    }

    pub fn warm(self: &Arc<Self>, user: InvokingUser, mirror: Option<String>) {
        let installed = Arc::clone(self);
        tokio::spawn(async move {
            let _ = installed
                .listing(&user, mirror.as_deref(), &Scope::root())
                .await;
        });
    }

    pub fn warm_enrolled(self: &Arc<Self>, mirror: Option<String>) {
        let installed = Arc::clone(self);
        tokio::spawn(async move {
            let scope = Scope::root();
            for user in crate::effect::accounts::group_members(MIX_USERS_GROUP) {
                let _ = installed.listing(&user, mirror.as_deref(), &scope).await;
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mix_core::declared::paths::{HOME_MANAGER_PROFILE_NAME, nix_profiles_dir};

    use super::*;

    #[derive(Default)]
    struct Counted {
        gathered: AtomicUsize,
        fail: Mutex<usize>,
        gate: Option<Arc<tokio::sync::Notify>>,
    }

    impl Source for Counted {
        fn gather<'a>(&'a self, ask: Ask<'a>) -> Gathering<'a> {
            Box::pin(async move {
                self.gathered.fetch_add(1, Ordering::SeqCst);
                if let Some(gate) = &self.gate {
                    gate.notified().await;
                }
                let mut fail = self.fail.lock().unwrap();
                if *fail > 0 {
                    *fail -= 1;
                    return Err(Error::Command {
                        command: "nix eval".to_string(),
                        detail: "no".to_string(),
                    });
                }
                Ok(vec![InstalledPackage {
                    name: ask.generation.display().to_string(),
                    ..InstalledPackage::default()
                }])
            })
        }
    }

    fn user(home: &Path) -> InvokingUser {
        InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "mix-user".to_string(),
            home: home.to_path_buf(),
        }
    }

    fn activate(home: &Path, generation: &str) -> PathBuf {
        activate_in(home, &home.join("store"), generation)
    }

    fn activate_in(home: &Path, store: &Path, generation: &str) -> PathBuf {
        let store = store.join(generation);
        std::fs::create_dir_all(&store).unwrap();
        let profiles = nix_profiles_dir(home);
        std::fs::create_dir_all(&profiles).unwrap();
        let link = profiles.join(HOME_MANAGER_PROFILE_NAME);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&store, &link).unwrap();
        std::fs::canonicalize(store).unwrap()
    }

    fn installed(source: &Arc<Counted>, home: &Path) -> Installed {
        std::fs::create_dir_all(home.join("store")).unwrap();
        Installed::with_source(Arc::clone(source) as Arc<dyn Source>)
            .in_store(std::fs::canonicalize(home.join("store")).unwrap())
    }

    #[tokio::test]
    async fn a_user_with_no_generation_has_nothing_to_list_and_nothing_is_asked() {
        let home = tempfile::tempdir().unwrap();
        let source = Arc::new(Counted::default());

        let listing = installed(&source, home.path())
            .listing(&user(home.path()), None, &Scope::root())
            .await
            .unwrap();

        assert!(listing.is_none());
        assert_eq!(source.gathered.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_second_listing_of_a_generation_is_answered_from_memory() {
        let home = tempfile::tempdir().unwrap();
        let generation = activate(home.path(), "one");
        let source = Arc::new(Counted::default());
        let installed = installed(&source, home.path());
        let scope = Scope::root();

        let first = installed
            .listing(&user(home.path()), None, &scope)
            .await
            .unwrap()
            .unwrap();
        let second = installed
            .listing(&user(home.path()), None, &scope)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(source.gathered.load(Ordering::SeqCst), 1);
        assert_eq!(first.generation, generation);
        assert!(Arc::ptr_eq(&first.packages, &second.packages));
    }

    #[tokio::test]
    async fn a_new_generation_is_gathered_again_and_the_old_answer_is_dropped() {
        let home = tempfile::tempdir().unwrap();
        activate(home.path(), "one");
        let source = Arc::new(Counted::default());
        let installed = installed(&source, home.path());
        let scope = Scope::root();
        installed
            .listing(&user(home.path()), None, &scope)
            .await
            .unwrap();

        let next = activate(home.path(), "two");
        let listed = installed
            .listing(&user(home.path()), None, &scope)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(source.gathered.load(Ordering::SeqCst), 2);
        assert_eq!(listed.generation, next);
        assert_eq!(listed.packages[0].name, next.display().to_string());
    }

    #[tokio::test]
    async fn a_failed_gather_is_not_remembered() {
        let home = tempfile::tempdir().unwrap();
        activate(home.path(), "one");
        let source = Arc::new(Counted {
            fail: Mutex::new(1),
            ..Counted::default()
        });
        let installed = installed(&source, home.path());
        let scope = Scope::root();

        assert!(
            installed
                .listing(&user(home.path()), None, &scope)
                .await
                .is_err()
        );
        let retried = installed
            .listing(&user(home.path()), None, &scope)
            .await
            .unwrap();

        assert!(retried.is_some());
        assert_eq!(source.gathered.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn listings_that_arrive_together_share_one_gather() {
        let home = tempfile::tempdir().unwrap();
        activate(home.path(), "one");
        let gate = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(Counted {
            gate: Some(Arc::clone(&gate)),
            ..Counted::default()
        });
        let installed = Arc::new(installed(&source, home.path()));
        let who = user(home.path());
        let listings: Vec<_> = (0..4)
            .map(|_| {
                let installed = Arc::clone(&installed);
                let who = who.clone();
                tokio::spawn(async move { installed.listing(&who, None, &Scope::root()).await })
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
    async fn a_listing_stopped_while_it_waits_for_another_ends_cancelled() {
        let home = tempfile::tempdir().unwrap();
        activate(home.path(), "one");
        let gate = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(Counted {
            gate: Some(Arc::clone(&gate)),
            ..Counted::default()
        });
        let installed = Arc::new(installed(&source, home.path()));
        let who = user(home.path());
        let first = {
            let installed = Arc::clone(&installed);
            let who = who.clone();
            tokio::spawn(async move { installed.listing(&who, None, &Scope::root()).await })
        };
        while source.gathered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let scope = Scope::root();
        scope.cancel(mix_exec::Reason::Interrupted);

        let stopped = installed.listing(&who, None, &scope).await;

        assert!(matches!(stopped, Err(Error::Cancelled { .. })));
        gate.notify_one();
        assert!(first.await.unwrap().unwrap().is_some());
    }

    #[tokio::test]
    async fn each_user_has_a_record_of_their_own() {
        let alice = tempfile::tempdir().unwrap();
        let bob = tempfile::tempdir().unwrap();
        let store = alice.path().join("store");
        activate_in(alice.path(), &store, "alice-one");
        activate_in(bob.path(), &store, "bob-one");
        let source = Arc::new(Counted::default());
        let installed = installed(&source, alice.path());
        let scope = Scope::root();
        let mut other = user(bob.path());
        other.uid = 1001;

        installed
            .listing(&user(alice.path()), None, &scope)
            .await
            .unwrap();
        installed.listing(&other, None, &scope).await.unwrap();

        assert_eq!(source.gathered.load(Ordering::SeqCst), 2);
    }
}
