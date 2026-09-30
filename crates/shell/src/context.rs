use std::path::PathBuf;

use mix_core::models::UserConfig;
use mix_core::policy::{Mirror, Policy};
use mix_exec::Scope;

#[derive(Debug, Clone, Default)]
pub struct HostConfig {
    pub git_binary: Option<PathBuf>,
}

pub struct Context {
    pub user: Option<UserConfig>,
    pub scope: Scope,
    pub policy: Policy,
    pub host: HostConfig,
    pub render: crate::render::Shared,
}

impl Context {
    pub fn new(scope: Scope) -> Self {
        Self {
            user: None,
            scope,
            policy: Policy::default(),
            host: HostConfig::default(),
            render: crate::render::shared(crate::render::Quiet),
        }
    }

    pub fn with_user(mut self, user: Option<UserConfig>) -> Self {
        self.user = user;
        self
    }

    pub fn with_render(mut self, render: impl crate::render::Render + 'static) -> Self {
        self.render = crate::render::shared(render);
        self
    }

    pub fn with_policy(mut self, policy: Policy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_host(mut self, host: HostConfig) -> Self {
        self.host = host;
        self
    }

    pub(crate) fn mirror(&self) -> Option<&str> {
        self.policy.mirror().map(Mirror::url)
    }
}
