use std::future::Future;

use mix_core::CancellationToken;
use nix::sys::signal::Signal;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinHandle;

pub struct Stopping {
    pub first: &'static str,
    pub forced: &'static str,
}

pub const BOOTSTRAP: Stopping = Stopping {
    first: "Cancelling... (cleaning up). Press Ctrl-C again to stop now.",
    forced: "Stopped before the cleanup finished. Run `mix bootstrap` again to finish it.",
};

pub const REPAIR: Stopping = Stopping {
    first: "Stopping after the current repair... Press Ctrl-C again to stop now.",
    forced: "Stopped in the middle of a repair. Run `mix repair` again to finish it.",
};

pub const CHANGE: Stopping = Stopping {
    first: "Cancelling... (putting the package list back). Press Ctrl-C again to stop now.",
    forced: "Stopped before the package list was put back. The next mix command puts it back.",
};

const FORCED_EXIT: i32 = 130;

pub struct Watch(JoinHandle<()>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn listen(signal_number: Signal) -> tokio::signal::unix::Signal {
    signal(SignalKind::from_raw(signal_number as i32))
        .unwrap_or_else(|error| panic!("registering a {signal_number} handler: {error}"))
}

pub fn watch(
    cancel: &CancellationToken,
    stopping: Stopping,
    client_gone: impl Future<Output = ()> + Send + 'static,
) -> Watch {
    let mut interrupt = listen(Signal::SIGINT);
    let mut terminate = listen(Signal::SIGTERM);
    let mut suspend = listen(Signal::SIGTSTP);
    let mut resume = listen(Signal::SIGCONT);
    let cancel = cancel.clone();
    Watch(tokio::spawn(async move {
        let mut client_gone = std::pin::pin!(client_gone);
        let mut client_left = false;
        loop {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
                () = &mut client_gone, if !client_left => {
                    client_left = true;
                    if cancel.is_cancelled() {
                        continue;
                    }
                }
                _ = suspend.recv() => {
                    mix_exec::group::pause_all();
                    let _ = nix::sys::signal::raise(Signal::SIGSTOP);
                    continue;
                }
                _ = resume.recv() => {
                    mix_exec::group::resume_all();
                    continue;
                }
            }
            if cancel.is_cancelled() {
                tracing::warn!("{}", stopping.forced);
                mix_exec::group::kill_all();
                mix_ui::restore_terminal();
                std::process::exit(FORCED_EXIT);
            }
            tracing::warn!("{}", stopping.first);
            cancel.cancel();
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nix::sys::signal;

    use super::*;

    static ONE_WATCH_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn cancelled_within_a_few_seconds(cancel: &CancellationToken) {
        tokio::time::timeout(Duration::from_secs(5), cancel.cancelled())
            .await
            .expect("the token should be cancelled");
    }

    #[tokio::test]
    async fn sigint_cancels_the_token() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let cancel = mix_exec::cancel::root();
        let _watch = watch(&cancel, BOOTSTRAP, std::future::pending());

        signal::raise(Signal::SIGINT).unwrap();

        cancelled_within_a_few_seconds(&cancel).await;
    }

    #[tokio::test]
    async fn sigterm_cancels_the_token() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let cancel = mix_exec::cancel::root();
        let _watch = watch(&cancel, BOOTSTRAP, std::future::pending());

        signal::raise(Signal::SIGTERM).unwrap();

        cancelled_within_a_few_seconds(&cancel).await;
    }

    #[tokio::test]
    async fn a_client_that_leaves_cancels_the_token() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let cancel = mix_exec::cancel::root();
        let (leave, left) = tokio::sync::oneshot::channel::<()>();
        let _watch = watch(&cancel, BOOTSTRAP, async {
            let _ = left.await;
        });

        drop(leave);

        cancelled_within_a_few_seconds(&cancel).await;
    }

    #[tokio::test]
    async fn a_client_that_leaves_after_an_interrupt_does_not_count_as_a_second_one() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let cancel = mix_exec::cancel::root();
        let (leave, left) = tokio::sync::oneshot::channel::<()>();
        let watch = watch(&cancel, BOOTSTRAP, async {
            let _ = left.await;
        });
        signal::raise(Signal::SIGINT).unwrap();
        cancelled_within_a_few_seconds(&cancel).await;

        drop(leave);
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(!watch.0.is_finished());
    }

    #[tokio::test]
    async fn a_finished_watch_cancels_nothing() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let cancel = mix_exec::cancel::root();
        let (leave, left) = tokio::sync::oneshot::channel::<()>();
        let watch = watch(&cancel, BOOTSTRAP, async {
            let _ = left.await;
        });

        drop(watch);
        drop(leave);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(!cancel.is_cancelled());
    }
}
