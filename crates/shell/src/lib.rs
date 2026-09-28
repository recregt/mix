//! What `mix` does, in three layers.
//!
//! At the top, one module per command — [`bootstrap`], [`install`], [`remove`], [`doctor`], [`repair`].
//! A command decides what happens and in which order, and nothing else calls into it: the two
//! that need the same work done reach for the same layer below rather than for each other.
//!
//! Under them, the two things that work is done to: a user's [`profile`], which is rendered and
//! activated, and a declared [`target`], which is measured and reconciled. This is where the
//! logic that more than one command needs lives.
//!
//! At the bottom, the system surfaces a layer above talks to — a process ([`exec`]), a file
//! ([`fs`]), a unit ([`systemd`]), a repository ([`git`]), a [`mirror`] — each named for what it
//! talks to rather than for who happens to share it.

mod exec;
mod fs;
mod git;
mod mirror;
mod systemd;

pub mod accounts;
pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod lock;
pub mod profile;
pub mod remove;
pub mod repair;
pub mod target;

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

#[doc(hidden)]
pub use exec::output;
