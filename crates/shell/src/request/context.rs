use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use mix_core::declared::identity::{InvokingUser, MIX_USERS_GROUP};
use mix_core::declared::paths::{
    FLAKE_LOCK, HOME_NIX, NIX_OWNERSHIP_MARKER, NIX_STORE, NIXOS_MARKER, STATE_FILE, mix_state_dir,
};
use mix_core::declared::policy::{Mirror, Policy};
use mix_core::declared::targets::UserConfig;
use mix_core::effect::{Fact, Failure, Query};
use mix_core::model::World;
use mix_events::Outbox;
use mix_exec::Scope;
use mix_fs::Files;
use mix_home::{Active, Session, myself};

use crate::drive::Performer;
use crate::request::Locked;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HomeFiles {
    pub state: Option<String>,
    pub active: Option<String>,
    pub lock: Option<String>,
    pub active_lock: Option<String>,
    pub home_nix: Option<String>,
}

#[derive(Clone, Default)]
pub enum Host {
    #[default]
    Machine,
    Model(Arc<Mutex<World>>),
}

impl Host {
    fn world(world: &Mutex<World>) -> std::sync::MutexGuard<'_, World> {
        world.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    pub async fn home_files(
        &self,
        user: &InvokingUser,
        scope: &Scope,
    ) -> Result<HomeFiles, Failure> {
        let state_dir = mix_state_dir(&user.home);
        let queries = [
            Query::Contents(state_dir.join(STATE_FILE)),
            Query::ActiveList(user.clone()),
            Query::Contents(state_dir.join(FLAKE_LOCK)),
            Query::ActiveLock(user.clone()),
            Query::Contents(state_dir.join(HOME_NIX)),
        ];
        let facts = match self {
            Host::Machine => {
                let mut session = Session::open(&myself(), user, &request_id(), scope)?;
                let mut facts = Vec::with_capacity(queries.len());
                let mut home = session.home();
                for query in &queries {
                    facts.push(home.observe(query).await?);
                }
                session.close().await?;
                facts
            }
            Host::Model(world) => {
                let world = Self::world(world);
                queries
                    .iter()
                    .map(|query| Some(world.observe(query)))
                    .collect()
            }
        };
        let text = |fact: &Option<Fact>| match fact {
            Some(Fact::Contents(Some(bytes))) => Some(Self::text(bytes)),
            _ => None,
        };
        Ok(HomeFiles {
            state: text(&facts[0]),
            active: text(&facts[1]),
            lock: text(&facts[2]),
            active_lock: text(&facts[3]),
            home_nix: text(&facts[4]),
        })
    }

    pub async fn active_generation(
        &self,
        user: &InvokingUser,
        scope: &Scope,
    ) -> Result<Option<Active>, Failure> {
        match self {
            Host::Machine => {
                let mut session = Session::open(&myself(), user, &request_id(), scope)?;
                let active = session.home().active(user).await?;
                session.close().await?;
                Ok(active)
            }
            Host::Model(world) => {
                let world = Self::world(world);
                let Some(profile) = world.profile(user) else {
                    return Ok(None);
                };
                let Some(number) = profile.active else {
                    return Ok(None);
                };
                if profile.dangling.contains(&number) {
                    return Ok(None);
                }
                Ok(Some(Active {
                    number,
                    path: Self::stored(profile.built.get(&number)),
                    list: world.active_list(user).map(Self::text),
                    lock: world.active_lock(user).map(Self::text),
                }))
            }
        }
    }

    fn stored(built: Option<&Vec<Option<Arc<[u8]>>>>) -> PathBuf {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        built.hash(&mut hasher);
        Path::new(NIX_STORE).join(format!("{:016x}-home-manager-generation", hasher.finish()))
    }

    pub fn available(&self) -> Option<u64> {
        match self {
            Host::Machine => {
                let stat = rustix::fs::statvfs(mix_core::declared::paths::NIX_STORE).ok()?;
                Some(stat.f_bavail.saturating_mul(stat.f_frsize))
            }
            Host::Model(_) => None,
        }
    }

    pub fn is_root(&self) -> bool {
        match self {
            Host::Machine => crate::effect::accounts::is_root(),
            Host::Model(_) => true,
        }
    }

    pub async fn preflight(&self, force: bool, scope: &Scope) -> crate::ops::bootstrap::Result<()> {
        use crate::ops::bootstrap::Error;
        match self {
            Host::Machine => crate::ops::bootstrap::preflight::check(force, scope).await,
            Host::Model(world) => {
                let world = Self::world(world);
                if world.files.contains_key(Path::new(NIXOS_MARKER)) {
                    return Err(Error::UnsupportedHost);
                }
                if !force
                    && world.contents(NIX_OWNERSHIP_MARKER).is_none()
                    && world.files.contains_key(Path::new(NIX_STORE))
                {
                    return Err(Error::AlreadyManaged);
                }
                Ok(())
            }
        }
    }

    pub fn enrolled(&self) -> Vec<InvokingUser> {
        match self {
            Host::Machine => crate::effect::accounts::group_members(MIX_USERS_GROUP),
            Host::Model(world) => {
                let world = Self::world(world);
                let Some(group) = world.groups.get(MIX_USERS_GROUP) else {
                    return Vec::new();
                };
                group
                    .members
                    .iter()
                    .filter_map(|name| {
                        let found = world.users.get(name)?;
                        Some(InvokingUser {
                            uid: found.uid,
                            gid: found.gid,
                            name: name.clone(),
                            home: found.home.clone(),
                        })
                    })
                    .collect()
            }
        }
    }

    pub fn regular_accounts(&self) -> Vec<InvokingUser> {
        match self {
            Host::Machine => crate::effect::accounts::regular_accounts(),
            Host::Model(world) => {
                let range = crate::effect::accounts::uid_range("");
                let world = Self::world(world);
                let builders = world
                    .groups
                    .get(mix_core::declared::identity::NIXBLD_GROUP)
                    .map(|group| group.gid);
                world
                    .users
                    .iter()
                    .filter(|(_, found)| range.contains(&found.uid))
                    .filter(|(_, found)| Some(found.gid) != builders)
                    .map(|(name, found)| InvokingUser {
                        uid: found.uid,
                        gid: found.gid,
                        name: name.clone(),
                        home: found.home.clone(),
                    })
                    .collect()
            }
        }
    }

    pub fn finish_removal(&self) -> Vec<Failure> {
        match self {
            Host::Machine => crate::effect::removal::finish(),
            Host::Model(world) => {
                let mut world = Self::world(world);
                for path in crate::effect::removal::leftovers() {
                    world.files.remove(path);
                }
                world.logs.clear();
                Vec::new()
            }
        }
    }

    pub fn is_member(&self, group: &str, user: &str) -> bool {
        match self {
            Host::Machine => crate::effect::accounts::group_has_member(group, user),
            Host::Model(world) => Self::world(world)
                .groups
                .get(group)
                .is_some_and(|found| found.members.iter().any(|member| member == user)),
        }
    }
}

pub fn request_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub struct Request {
    pub id: String,
    pub outbox: Arc<Outbox>,
}

pub struct Context {
    pub request: Request,
    pub user: Option<UserConfig>,
    pub caller_is_root: bool,
    pub scope: Scope,
    pub policy: Policy,
    pub render: crate::request::sink::Shared,
    pub locked: Locked,
    pub journals: std::path::PathBuf,
    pub dry_run: bool,
    pub host: Host,
    pub installed: Arc<crate::request::installed::Installed>,
    pub upstream: Arc<dyn crate::request::upstream::Upstream>,
    pub faults: Option<Arc<crate::drive::Faults>>,
}

impl Context {
    pub(crate) fn journal(
        &self,
        dir: &Path,
    ) -> Result<crate::effect::journal::RequestJournal, mix_core::effect::Failure> {
        use crate::effect::journal::{FileJournal, ModelJournal, RequestJournal};
        Ok(match &self.host {
            Host::Machine => RequestJournal::File(FileJournal::create(dir, &self.request.id)?),
            Host::Model(world) => {
                RequestJournal::Model(ModelJournal::open(Arc::clone(world), &self.request.id))
            }
        })
    }

    pub(crate) fn release(&self) {
        if let Host::Model(world) = &self.host {
            world
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .forget(&self.request.id);
        }
    }

    pub(crate) fn interrupted(&self, dir: &Path) -> bool {
        match &self.host {
            Host::Machine => !crate::effect::journal::abandoned(dir).is_empty(),
            Host::Model(world) => {
                let world = world.lock().unwrap_or_else(PoisonError::into_inner);
                world
                    .logs
                    .keys()
                    .any(|request| !world.held.contains(request))
            }
        }
    }

    pub(crate) async fn recover(
        &self,
        dir: &Path,
        performer: &mut Performer,
        scope: &mix_exec::Scope,
        losing: bool,
    ) -> crate::effect::journal::Recovered {
        use crate::effect::journal::{
            predict_recovery, recover_accepting_loss, recover_all, recover_model,
        };
        match performer.model() {
            Some(world) => recover_model(&world, performer, scope, losing).await,
            None if self.dry_run => predict_recovery(dir, performer, scope).await,
            None if losing => recover_accepting_loss(dir, performer, scope).await,
            None => recover_all(dir, performer, scope).await,
        }
    }

    pub(crate) fn performer(&self) -> std::io::Result<Performer> {
        match &self.host {
            Host::Machine => {
                let files = Files::open(Path::new("/"), &self.request.id)?;
                Ok(if self.dry_run {
                    Performer::predicting(files)
                } else {
                    Performer::new(files)
                })
            }
            Host::Model(world) if self.dry_run => {
                let copy = world.lock().unwrap_or_else(PoisonError::into_inner).clone();
                Ok(Performer::modelled(Arc::new(Mutex::new(copy)))?.acting_for(&self.request.id))
            }
            Host::Model(world) => Ok(Performer::modelled(Arc::clone(world))?
                .acting_for(&self.request.id)
                .with_faults(self.faults.clone())),
        }
    }

    pub(crate) fn relay(&self) -> crate::request::sink::Relay {
        crate::request::sink::Relay::new(Arc::clone(&self.render))
    }

    pub(crate) fn mirror(&self) -> Option<&str> {
        self.policy.mirror().map(Mirror::url)
    }
}
