use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio_util::sync::CancellationToken;

use crate::group::ProcessSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
}

#[derive(Debug, Default)]
struct Flag {
    raised: AtomicBool,
    parent: Option<Arc<Flag>>,
}

impl Flag {
    #[inline]
    fn is_raised(&self) -> bool {
        let mut flag = Some(self);
        while let Some(current) = flag {
            if current.raised.load(Ordering::Acquire) {
                return true;
            }
            flag = current.parent.as_deref();
        }
        false
    }
}

#[derive(Debug, Clone)]
pub struct Scope {
    token: CancellationToken,
    flag: Arc<Flag>,
    processes: ProcessSet,
}

impl Scope {
    #[allow(clippy::disallowed_methods)]
    fn with_processes(processes: ProcessSet) -> Self {
        Self {
            token: CancellationToken::new(),
            flag: Arc::default(),
            processes,
        }
    }

    pub fn root() -> Self {
        Self::with_processes(ProcessSet::default())
    }

    pub fn child(&self) -> Self {
        Self {
            token: self.token.child_token(),
            flag: Arc::new(Flag {
                raised: AtomicBool::new(false),
                parent: Some(Arc::clone(&self.flag)),
            }),
            processes: self.processes.clone(),
        }
    }

    pub fn shielded(&self) -> Self {
        Self::with_processes(self.processes.clone())
    }

    pub fn cancel(&self) {
        self.flag.raised.store(true, Ordering::Release);
        self.token.cancel();
    }

    #[inline]
    pub fn is_stopped(&self) -> bool {
        self.flag.is_raised()
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

        root.cancel();

        assert!(child.is_stopped());
        assert!(!shield.is_stopped());
    }

    #[test]
    fn a_grandchild_is_stopped_by_its_root_and_not_by_a_sibling() {
        let root = Scope::root();
        let child = root.child();
        let grandchild = child.child();
        let sibling = root.child();

        sibling.cancel();
        assert!(!grandchild.is_stopped());

        root.cancel();
        assert!(grandchild.is_stopped());
        assert!(!root.shielded().is_stopped());
    }

    #[tokio::test]
    async fn a_cancelled_parent_wakes_a_child_that_waits() {
        let root = Scope::root();
        let child = root.child();
        let waiting = tokio::spawn(async move { child.stopped().await });

        root.cancel();

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
        scope.cancel();

        assert_eq!(
            scope.guard(std::future::pending::<()>()).await,
            Err(Stop::Cancelled)
        );
    }
}
