use std::path::PathBuf;
use std::sync::Arc;

use mix_core::policy::{Mirror, Policy};
use mix_core::targets::UserConfig;
use mix_events::Outbox;
use mix_exec::Scope;

use crate::request::Locked;

#[derive(Debug, Clone, Default)]
pub struct HostConfig {
    pub git_binary: Option<PathBuf>,
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
    pub host: HostConfig,
    pub render: crate::request::sink::Shared,
    pub locked: Locked,
}

impl Context {
    pub(crate) fn relay(&self) -> crate::request::sink::Relay {
        crate::request::sink::Relay::new(Arc::clone(&self.render))
    }

    pub(crate) fn mirror(&self) -> Option<&str> {
        self.policy.mirror().map(Mirror::url)
    }
}
