use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use mix_core::identity::InvokingUser;
use mix_core::paths::{
    HOME_NIX, NIX_OWNERSHIP_MARKER, NIX_STORE, NIXOS_MARKER, STATE_FILE, mix_state_dir,
};
use mix_core::policy::{Mirror, Policy};
use mix_core::targets::UserConfig;
use mix_core::world::World;
use mix_events::Outbox;
use mix_exec::Scope;

use crate::drive::Performer;
use crate::effect::files::Files;
use crate::request::Locked;

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

    fn read(&self, path: &Path) -> Option<String> {
        match self {
            Host::Machine => crate::profile::state::read(path),
            Host::Model(world) => Self::world(world)
                .contents(path)
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    pub fn state_file(&self, user: &InvokingUser) -> Option<String> {
        self.read(&mix_state_dir(&user.home).join(STATE_FILE))
    }

    pub fn home_nix(&self, user: &InvokingUser) -> Option<String> {
        self.read(&mix_state_dir(&user.home).join(HOME_NIX))
    }

    pub fn active_list(&self, user: &InvokingUser) -> Option<String> {
        match self {
            Host::Machine => crate::profile::state::read(
                &crate::profile::state::active_generation_state(&user.home),
            ),
            Host::Model(world) => Self::world(world)
                .active_list(user)
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    pub fn available(&self) -> Option<u64> {
        match self {
            Host::Machine => {
                let stat = rustix::fs::statvfs(mix_core::paths::NIX_STORE).ok()?;
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
    pub faults: Option<Arc<crate::drive::Faults>>,
}

impl Context {
    pub(crate) fn journal(
        &self,
        dir: &Path,
    ) -> Result<crate::effect::journal::RequestJournal, mix_core::action::Failure> {
        use crate::effect::journal::{FileJournal, ModelJournal, RequestJournal};
        Ok(match &self.host {
            Host::Machine => RequestJournal::File(FileJournal::create(dir, &self.request.id)?),
            Host::Model(world) => {
                RequestJournal::Model(ModelJournal::open(Arc::clone(world), &self.request.id))
            }
        })
    }

    pub(crate) fn interrupted(&self, dir: &Path) -> bool {
        match &self.host {
            Host::Machine => !crate::effect::journal::unfinished(dir).is_empty(),
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
