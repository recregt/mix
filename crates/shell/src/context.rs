use std::path::PathBuf;
use std::sync::Arc;

use mix_core::models::UserConfig;
use mix_core::{
    ActivityReporter, DownloadProgress, NoopActivity, NoopProgress, NoopSteps, Scope, StepObserver,
};

#[derive(Clone)]
pub struct Reporters {
    pub downloads: Arc<dyn DownloadProgress>,
    pub steps: Arc<dyn StepObserver>,
    pub activity: Arc<dyn ActivityReporter>,
}

impl Reporters {
    pub fn silent() -> Self {
        Self {
            downloads: Arc::new(NoopProgress),
            steps: Arc::new(NoopSteps),
            activity: Arc::new(NoopActivity),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HostConfig {
    pub git_binary: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct RequestEnv {
    pub mirror: Option<String>,
    pub mirror_key: Option<String>,
}

pub struct Context {
    pub user: Option<UserConfig>,
    pub scope: Scope,
    pub reporters: Reporters,
    pub env: RequestEnv,
    pub host: HostConfig,
}

impl Context {
    pub fn new(scope: Scope) -> Self {
        Self {
            user: None,
            scope,
            reporters: Reporters::silent(),
            env: RequestEnv::default(),
            host: HostConfig::default(),
        }
    }

    pub fn with_user(mut self, user: Option<UserConfig>) -> Self {
        self.user = user;
        self
    }

    pub fn with_reporters(mut self, reporters: Reporters) -> Self {
        self.reporters = reporters;
        self
    }

    pub fn with_env(mut self, env: RequestEnv) -> Self {
        self.env = env;
        self
    }

    pub fn with_host(mut self, host: HostConfig) -> Self {
        self.host = host;
        self
    }

    pub(crate) fn mirror(&self) -> Option<&str> {
        self.env.mirror.as_deref()
    }

    pub(crate) fn mirror_key(&self) -> Option<&str> {
        self.env.mirror_key.as_deref()
    }
}
