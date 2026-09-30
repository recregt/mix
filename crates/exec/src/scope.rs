use std::future::Future;
use std::sync::{Arc, OnceLock};

use tokio_util::sync::CancellationToken;

use crate::group::ProcessSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Interrupted,
    Terminated,
    ClientGone,
    Abandoned,
}

#[derive(Debug, Default)]
struct Flag {
    reason: OnceLock<Reason>,
    parent: Option<Arc<Flag>>,
}

impl Flag {
    #[inline]
    fn reason(&self) -> Option<Reason> {
        let mut flag = Some(self);
        while let Some(current) = flag {
            if let Some(reason) = current.reason.get() {
                return Some(*reason);
            }
            flag = current.parent.as_deref();
        }
        None
    }
}

pub trait Watch: Send + Sync {
    fn started(&self, command: &str);
}

#[derive(Clone)]
pub struct Scope {
    token: CancellationToken,
    flag: Arc<Flag>,
    processes: ProcessSet,
    watch: Option<Arc<dyn Watch>>,
}

impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scope")
            .field("token", &self.token)
            .field("flag", &self.flag)
            .field("processes", &self.processes)
            .field("watched", &self.watch.is_some())
            .finish()
    }
}

impl Scope {
    #[allow(clippy::disallowed_methods)]
    fn with_processes(processes: ProcessSet) -> Self {
        Self {
            token: CancellationToken::new(),
            flag: Arc::default(),
            processes,
            watch: None,
        }
    }

    pub fn root() -> Self {
        Self::with_processes(ProcessSet::default())
    }

    pub fn child(&self) -> Self {
        Self {
            token: self.token.child_token(),
            flag: Arc::new(Flag {
                reason: OnceLock::new(),
                parent: Some(Arc::clone(&self.flag)),
            }),
            processes: self.processes.clone(),
            watch: self.watch.clone(),
        }
    }

    pub fn shielded(&self) -> Self {
        Self {
            watch: self.watch.clone(),
            ..Self::with_processes(self.processes.clone())
        }
    }

    pub fn watched(&self, watch: Arc<dyn Watch>) -> Self {
        Self {
            watch: Some(watch),
            ..self.clone()
        }
    }

    pub(crate) fn started(&self, command: &str) {
        if let Some(watch) = &self.watch {
            watch.started(command);
        }
    }

    pub fn cancel(&self, reason: Reason) {
        let _ = self.flag.reason.set(reason);
        self.token.cancel();
    }

    #[inline]
    pub fn is_stopped(&self) -> bool {
        self.flag.reason().is_some()
    }

    #[inline]
    pub fn reason(&self) -> Option<Reason> {
        self.flag.reason()
    }

    pub async fn stopped(&self) -> Stop {
        self.token.cancelled().await;
        Stop::Cancelled
    }

    pub async fn guard<F: Future>(&self, work: F) -> Result<F::Output, Stop> {
        tokio::select! {
            biased;
            stop = self.stopped() => Err(stop),
            output = work => Ok(output),
        }
    }

    pub fn processes(&self) -> &ProcessSet {
        &self.processes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shield_is_not_reached_by_a_cancelled_parent() {
        let root = Scope::root();
        let child = root.child();
        let shield = root.shielded();

        root.cancel(Reason::Interrupted);

        assert!(child.is_stopped());
        assert!(!shield.is_stopped());
    }

    #[test]
    fn the_first_reason_is_kept_and_the_nearest_one_is_reported() {
        let root = Scope::root();
        let child = root.child();

        child.cancel(Reason::Abandoned);
        root.cancel(Reason::Terminated);
        root.cancel(Reason::ClientGone);

        assert_eq!(root.reason(), Some(Reason::Terminated));
        assert_eq!(child.reason(), Some(Reason::Abandoned));
        assert_eq!(root.child().reason(), Some(Reason::Terminated));
        assert_eq!(root.shielded().reason(), None);
    }

    #[test]
    fn a_grandchild_is_stopped_by_its_root_and_not_by_a_sibling() {
        let root = Scope::root();
        let child = root.child();
        let grandchild = child.child();
        let sibling = root.child();

        sibling.cancel(Reason::Interrupted);
        assert!(!grandchild.is_stopped());

        root.cancel(Reason::Interrupted);
        assert!(grandchild.is_stopped());
        assert!(!root.shielded().is_stopped());
    }

    #[tokio::test]
    async fn a_cancelled_parent_wakes_a_child_that_waits() {
        let root = Scope::root();
        let child = root.child();
        let waiting = tokio::spawn(async move { child.stopped().await });

        root.cancel(Reason::Interrupted);

        assert_eq!(waiting.await.unwrap(), Stop::Cancelled);
    }

    #[test]
    fn a_child_and_a_shield_share_the_processes_of_their_request() {
        let root = Scope::root();

        assert!(root.child().processes().same_set(root.processes()));
        assert!(root.shielded().processes().same_set(root.processes()));
        assert!(!Scope::root().processes().same_set(root.processes()));
    }

    #[tokio::test]
    async fn guard_returns_the_output_of_work_that_finishes() {
        assert_eq!(Scope::root().guard(async { 7 }).await, Ok(7));
    }

    #[tokio::test]
    async fn guard_stops_waiting_when_the_scope_is_cancelled() {
        let scope = Scope::root();
        scope.cancel(Reason::Interrupted);

        assert_eq!(
            scope.guard(std::future::pending::<()>()).await,
            Err(Stop::Cancelled)
        );
    }
}
