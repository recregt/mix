use std::future::Future;

use mix_core::CancellationToken;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinHandle;

pub const UNDOING: &str = "Cancelling... (cleaning up)";
pub const FINISHING: &str = "Stopping after the current repair...";

pub struct Watch(JoinHandle<()>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub fn watch(
    cancel: &CancellationToken,
    notice: &'static str,
    client_gone: impl Future<Output = ()> + Send + 'static,
) -> Watch {
    let mut interrupt = signal(SignalKind::interrupt()).expect("registering a SIGINT handler");
    let mut terminate = signal(SignalKind::terminate()).expect("registering a SIGTERM handler");
    let cancel = cancel.clone();
    Watch(tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
            () = client_gone => {}
        }
        tracing::warn!("{notice}");
        cancel.cancel();
    }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nix::sys::signal::{self, Signal};

    use super::*;

    async fn cancelled_within_a_few_seconds(cancel: &CancellationToken) {
        tokio::time::timeout(Duration::from_secs(5), cancel.cancelled())
            .await
            .expect("the token should be cancelled");
    }

    #[tokio::test]
    async fn sigint_cancels_the_token() {
        let cancel = CancellationToken::new();
        let _watch = watch(&cancel, UNDOING, std::future::pending());

        signal::raise(Signal::SIGINT).unwrap();

        cancelled_within_a_few_seconds(&cancel).await;
    }

    #[tokio::test]
    async fn sigterm_cancels_the_token() {
        let cancel = CancellationToken::new();
        let _watch = watch(&cancel, UNDOING, std::future::pending());

        signal::raise(Signal::SIGTERM).unwrap();

        cancelled_within_a_few_seconds(&cancel).await;
    }

    #[tokio::test]
    async fn a_client_that_leaves_cancels_the_token() {
        let cancel = CancellationToken::new();
        let (leave, left) = tokio::sync::oneshot::channel::<()>();
        let _watch = watch(&cancel, UNDOING, async {
            let _ = left.await;
        });

        drop(leave);

        cancelled_within_a_few_seconds(&cancel).await;
    }

    #[tokio::test]
    async fn a_finished_watch_cancels_nothing() {
        let cancel = CancellationToken::new();
        let (leave, left) = tokio::sync::oneshot::channel::<()>();
        let watch = watch(&cancel, UNDOING, async {
            let _ = left.await;
        });

        drop(watch);
        drop(leave);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(!cancel.is_cancelled());
    }
}
