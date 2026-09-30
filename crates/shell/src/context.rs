use std::path::PathBuf;
use std::sync::{Arc, PoisonError};

use mix_core::models::UserConfig;
use mix_core::policy::{Mirror, Policy};
use mix_events::Outbox;
use mix_exec::Scope;
use tokio::sync::Notify;

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
    pub(crate) woken: Arc<Notify>,
}

impl Request {
    fn new() -> Self {
        let id = request_id();
        let woken = Arc::new(Notify::new());
        let wake = Arc::clone(&woken);
        Self {
            outbox: Arc::new(Outbox::new(id.clone(), move || wake.notify_one())),
            id,
            woken,
        }
    }
}

pub struct Context {
    pub request: Request,
    pub user: Option<UserConfig>,
    pub scope: Scope,
    pub policy: Policy,
    pub host: HostConfig,
    pub render: crate::render::Shared,
}

impl Context {
    pub fn new(scope: Scope) -> Self {
        Self {
            request: Request::new(),
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

    pub fn span(&self) -> tracing::Span {
        let logs = self
            .render
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .logs();
        match logs {
            Some(level) => crate::logs::anchor(None, &self.request.outbox, 0, level),
            None => tracing::Span::none(),
        }
    }

    pub(crate) fn relay(&self) -> crate::render::Relay {
        crate::render::Relay::new(
            Arc::clone(&self.request.outbox),
            Arc::clone(&self.render),
            Arc::clone(&self.request.woken),
        )
    }

    pub(crate) fn mirror(&self) -> Option<&str> {
        self.policy.mirror().map(Mirror::url)
    }
}
