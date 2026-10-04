use std::future::Future;

use mix_exec::{Reason, Scope};
use mix_rpc::{Controls, Events, Reply};
use nix::sys::signal::Signal;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinHandle;

pub struct Watch(JoinHandle<()>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub fn listen(signal_number: Signal) -> tokio::signal::unix::Signal {
    signal(SignalKind::from_raw(signal_number as i32))
        .unwrap_or_else(|error| panic!("registering a {signal_number} handler: {error}"))
}

fn steered(scope: &Scope, control: mix_rpc::Control) {
    match control {
        mix_rpc::Control::Interrupt => scope.cancel(Reason::Interrupted),
        mix_rpc::Control::Terminate => scope.cancel(Reason::Terminated),
        mix_rpc::Control::Pause => scope.processes().pause(),
        mix_rpc::Control::Resume => scope.processes().resume(),
    }
}

pub fn steer(scope: &Scope, mut controls: Controls, events: &Events) -> Watch {
    let scope = scope.clone();
    let events = events.clone();
    let mut terminate = listen(Signal::SIGTERM);
    Watch(tokio::spawn(async move {
        let mut steering = true;
        loop {
            tokio::select! {
                control = controls.recv(), if steering => match control {
                    Some(control) => {
                        steered(&scope, control);
                        let _ = events.send(Reply::Applied(control));
                    }
                    None => steering = false,
                },
                () = events.closed() => {
                    scope.cancel(Reason::ClientGone);
                    return;
                }
                _ = terminate.recv() => scope.cancel(Reason::Terminated),
            }
        }
    }))
}

pub fn ignore_the_terminal() -> impl Future<Output = ()> + Send + 'static {
    let mut interrupt = listen(Signal::SIGINT);
    let mut suspend = listen(Signal::SIGTSTP);
    let mut hangup = listen(Signal::SIGHUP);
    async move {
        loop {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = suspend.recv() => {}
                _ = hangup.recv() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ONE_WATCH_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn steering() -> (
        Scope,
        tokio::sync::mpsc::UnboundedSender<mix_rpc::Control>,
        tokio::sync::mpsc::UnboundedReceiver<Reply>,
        Watch,
    ) {
        let scope = mix_exec::Scope::root();
        let (controls, steered) = tokio::sync::mpsc::unbounded_channel();
        let (events, replies) = tokio::sync::mpsc::unbounded_channel();
        let watch = steer(&scope, steered, &events);
        (scope, controls, replies, watch)
    }

    #[tokio::test]
    async fn a_control_from_the_client_is_applied_then_acknowledged() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let (scope, controls, mut replies, _watch) = steering();

        controls.send(mix_rpc::Control::Pause).unwrap();
        assert_eq!(
            replies.recv().await,
            Some(Reply::Applied(mix_rpc::Control::Pause))
        );
        assert!(!scope.is_stopped());

        controls.send(mix_rpc::Control::Interrupt).unwrap();
        assert_eq!(
            replies.recv().await,
            Some(Reply::Applied(mix_rpc::Control::Interrupt))
        );
        assert_eq!(scope.reason(), Some(Reason::Interrupted));
    }

    #[tokio::test]
    async fn a_client_that_leaves_cancels_the_request() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let (scope, _controls, replies, _watch) = steering();

        drop(replies);

        scope.stopped().await;
        assert_eq!(scope.reason(), Some(Reason::ClientGone));
    }

    #[tokio::test]
    async fn a_client_that_stops_sending_controls_has_not_left() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let (scope, controls, mut replies, _watch) = steering();

        drop(controls);
        tokio::task::yield_now().await;

        assert!(!scope.is_stopped());
        assert!(replies.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_finished_steer_cancels_nothing() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let (scope, _controls, replies, watch) = steering();

        drop(watch);
        drop(replies);
        tokio::task::yield_now().await;

        assert!(!scope.is_stopped());
    }
}
