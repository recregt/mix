use std::future::Future;

use mix_exec::Reason;
use mix_exec::Scope;
use mix_ui::{Help, Note, Phrase, help, note, phrase};
use nix::sys::signal::Signal;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct Stopping {
    pub first: Note,
    pub first_help: Help,
    pub forced: Phrase,
    pub forced_help: Help,
}

pub fn bootstrap() -> Stopping {
    Stopping {
        first: note!("cancelling and cleaning up"),
        first_help: help!("press Ctrl-C again to stop now"),
        forced: phrase!("stopped before the cleanup finished"),
        forced_help: help!("run `mix bootstrap` again to finish it"),
    }
}

pub fn repair() -> Stopping {
    Stopping {
        first: note!("stopping after the current repair"),
        first_help: help!("press Ctrl-C again to stop now"),
        forced: phrase!("stopped in the middle of a repair"),
        forced_help: help!("run `mix repair` again to finish it"),
    }
}

pub fn change() -> Stopping {
    Stopping {
        first: note!("cancelling and putting the package list back"),
        first_help: help!("press Ctrl-C again to stop now"),
        forced: phrase!("stopped before the package list was put back"),
        forced_help: help!("run any `mix` command to put it back"),
    }
}

const FORCED_EXIT: u32 = mix_events::exit::INTERRUPTED;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    Interrupt,
    Terminate,
    Suspend,
    Continue,
    ClientGone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Cancel(Reason),
    ForceStop,
    Pause,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Client,
    Worker,
}

#[derive(Debug, Default)]
pub struct Translator {
    cancelled: bool,
    client_left: bool,
}

impl Translator {
    pub fn translate(&mut self, received: Received) -> Option<Control> {
        match received {
            Received::Suspend => Some(Control::Pause),
            Received::Continue => Some(Control::Resume),
            Received::ClientGone => {
                let first = !self.client_left && !self.cancelled;
                self.client_left = true;
                self.cancelled = true;
                first.then_some(Control::Cancel(Reason::ClientGone))
            }
            Received::Interrupt | Received::Terminate => {
                if self.cancelled {
                    return Some(Control::ForceStop);
                }
                self.cancelled = true;
                Some(Control::Cancel(if received == Received::Interrupt {
                    Reason::Interrupted
                } else {
                    Reason::Terminated
                }))
            }
        }
    }
}

pub fn apply(scope: &Scope, notices: Option<&Stopping>, control: Control) {
    match control {
        Control::Cancel(reason) => scope.cancel(reason),
        Control::ForceStop => {
            if let Some(notices) = notices {
                mix_ui::report(
                    mix_ui::Severity::Error,
                    &mix_ui::Report::new(&notices.forced).help(&notices.forced_help),
                );
            }
            scope.processes().kill();
            mix_ui::restore_terminal();
            std::process::exit(i32::try_from(FORCED_EXIT).unwrap_or(i32::MAX));
        }
        Control::Pause => scope.processes().pause(),
        Control::Resume => scope.processes().resume(),
    }
}

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
    scope: &Scope,
    notices: Option<Stopping>,
    client_gone: impl Future<Output = ()> + Send + 'static,
    side: Side,
) -> Watch {
    let mut interrupt = listen(Signal::SIGINT);
    let mut terminate = listen(Signal::SIGTERM);
    let mut suspend = listen(Signal::SIGTSTP);
    let mut resume = listen(Signal::SIGCONT);
    let scope = scope.clone();
    Watch(tokio::spawn(async move {
        let mut client_gone = std::pin::pin!(client_gone);
        let mut client_left = false;
        let mut translator = Translator::default();
        loop {
            let received = tokio::select! {
                _ = interrupt.recv() => Received::Interrupt,
                _ = terminate.recv() => Received::Terminate,
                _ = suspend.recv() => Received::Suspend,
                _ = resume.recv() => Received::Continue,
                () = &mut client_gone, if !client_left => {
                    client_left = true;
                    Received::ClientGone
                }
            };
            let Some(control) = translator.translate(received) else {
                continue;
            };
            apply(&scope, notices.as_ref(), control);
            if control == Control::Pause && side == Side::Client {
                let _ = nix::sys::signal::raise(Signal::SIGSTOP);
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nix::sys::signal;

    use super::*;

    static ONE_WATCH_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn translated(received: &[Received]) -> Vec<Option<Control>> {
        let mut translator = Translator::default();
        received
            .iter()
            .map(|received| translator.translate(*received))
            .collect()
    }

    #[test]
    fn a_first_interrupt_cancels_and_a_second_forces_a_stop() {
        assert_eq!(
            translated(&[Received::Interrupt, Received::Interrupt]),
            [
                Some(Control::Cancel(Reason::Interrupted)),
                Some(Control::ForceStop)
            ]
        );
        assert_eq!(
            translated(&[Received::Terminate, Received::Interrupt]),
            [
                Some(Control::Cancel(Reason::Terminated)),
                Some(Control::ForceStop)
            ]
        );
    }

    #[test]
    fn a_client_that_leaves_cancels_once_and_never_forces_a_stop() {
        assert_eq!(
            translated(&[Received::ClientGone]),
            [Some(Control::Cancel(Reason::ClientGone))]
        );
        assert_eq!(
            translated(&[Received::Interrupt, Received::ClientGone]),
            [Some(Control::Cancel(Reason::Interrupted)), None]
        );
    }

    #[test]
    fn an_interrupt_after_the_client_left_forces_a_stop() {
        assert_eq!(
            translated(&[Received::ClientGone, Received::Interrupt]),
            [
                Some(Control::Cancel(Reason::ClientGone)),
                Some(Control::ForceStop)
            ]
        );
    }

    #[test]
    fn suspending_and_resuming_pause_and_resume_the_request() {
        assert_eq!(
            translated(&[Received::Suspend, Received::Continue]),
            [Some(Control::Pause), Some(Control::Resume)]
        );
    }

    #[test]
    fn suspending_does_not_count_as_an_interrupt() {
        assert_eq!(
            translated(&[Received::Suspend, Received::Interrupt]),
            [
                Some(Control::Pause),
                Some(Control::Cancel(Reason::Interrupted))
            ]
        );
    }

    #[test]
    fn cancelling_stops_the_scope_and_pausing_does_not() {
        let scope = mix_exec::Scope::root();

        apply(&scope, Some(&bootstrap()), Control::Pause);
        apply(&scope, Some(&bootstrap()), Control::Resume);
        assert!(!scope.is_stopped());

        apply(
            &scope,
            Some(&bootstrap()),
            Control::Cancel(Reason::Terminated),
        );
        assert_eq!(scope.reason(), Some(Reason::Terminated));
    }

    async fn cancelled_within_a_few_seconds(scope: &Scope) {
        tokio::time::timeout(Duration::from_secs(5), scope.stopped())
            .await
            .expect("the scope should be stopped");
    }

    #[tokio::test]
    async fn sigint_cancels_the_request() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let scope = mix_exec::Scope::root();
        let _watch = watch(
            &scope,
            Some(bootstrap()),
            std::future::pending(),
            Side::Client,
        );

        signal::raise(Signal::SIGINT).unwrap();

        cancelled_within_a_few_seconds(&scope).await;
    }

    #[tokio::test]
    async fn sigterm_cancels_the_request() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let scope = mix_exec::Scope::root();
        let _watch = watch(
            &scope,
            Some(bootstrap()),
            std::future::pending(),
            Side::Client,
        );

        signal::raise(Signal::SIGTERM).unwrap();

        cancelled_within_a_few_seconds(&scope).await;
        assert_eq!(scope.reason(), Some(Reason::Terminated));
    }

    #[tokio::test]
    async fn a_client_that_leaves_cancels_the_request() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let scope = mix_exec::Scope::root();
        let (leave, left) = tokio::sync::oneshot::channel::<()>();
        let _watch = watch(
            &scope,
            Some(bootstrap()),
            async {
                let _ = left.await;
            },
            Side::Worker,
        );

        drop(leave);

        cancelled_within_a_few_seconds(&scope).await;
        assert_eq!(scope.reason(), Some(Reason::ClientGone));
    }

    #[tokio::test]
    async fn a_finished_watch_cancels_nothing() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let scope = mix_exec::Scope::root();
        let (leave, left) = tokio::sync::oneshot::channel::<()>();
        let watch = watch(
            &scope,
            Some(bootstrap()),
            async {
                let _ = left.await;
            },
            Side::Worker,
        );

        drop(watch);
        drop(leave);
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(!scope.is_stopped());
    }
}
