use std::path::PathBuf;
use std::sync::Arc;

use mix_core::models::UserConfig;
use mix_core::policy::{Mirror, Policy};
use mix_events::Outbox;
use mix_exec::Scope;

use crate::request::Locked;

#[derive(Debug, Clone, Default)]
pub struct HostConfig {
    pub git_binary: Option<PathBuf>,
}

pub use mix_events::request_id;

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
    pub host: HostConfig,
    pub render: crate::render::Shared,
    pub locked: Locked,
}

impl Context {
    pub(crate) fn relay(&self) -> crate::render::Relay {
        crate::render::Relay::new(Arc::clone(&self.render))
    }

    pub(crate) fn mirror(&self) -> Option<&str> {
        self.policy.mirror().map(Mirror::url)
    }
}
