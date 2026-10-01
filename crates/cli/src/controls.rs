use std::future::Future;

use mix_exec::Reason;
use mix_exec::Scope;
use mix_rpc::{Controls, Events, Reply};
use mix_ui::{Help, Note, Phrase, help, note, phrase};
use nix::sys::signal::Signal;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub enum Then {
    Stop { forced: Phrase, help: Help },
    Detach,
}

#[derive(Debug)]
pub struct Stopping {
    pub first: Note,
    pub then: Then,
}

impl Stopping {
    pub fn first_help(&self) -> Help {
        match self.then {
            Then::Stop { .. } => help!("press Ctrl-C again to stop now"),
            Then::Detach => help!("press Ctrl-C again to leave it running in the background"),
        }
    }

    pub fn detached(self) -> Self {
        Self {
            then: Then::Detach,
            ..self
        }
    }
}

pub fn bootstrap() -> Stopping {
    Stopping {
        first: note!("cancelling and cleaning up"),
        then: Then::Stop {
            forced: phrase!("stopped before the cleanup finished"),
            help: help!("run `mix bootstrap` again to finish it"),
        },
    }
}

pub fn repair() -> Stopping {
    Stopping {
        first: note!("stopping after the current repair"),
        then: Then::Stop {
            forced: phrase!("stopped in the middle of a repair"),
            help: help!("run `mix repair` again to finish it"),
        },
    }
}

pub fn change() -> Stopping {
    Stopping {
        first: note!("cancelling and putting the package list back"),
        then: Then::Stop {
            forced: phrase!("stopped before the package list was put back"),
            help: help!("run any `mix` command to put it back"),
        },
    }
}

pub fn detached() -> Note {
    note!("`mix` is finishing the cleanup in the background")
}

pub const FORCED_EXIT: u32 = mix_events::exit::INTERRUPTED;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    Interrupt,
    Terminate,
    Suspend,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Cancel(Reason),
    ForceStop,
    Pause,
    Resume,
}

#[derive(Debug, Default)]
pub struct Translator {
    cancelled: bool,
}

impl Translator {
    pub fn translate(&mut self, received: Received) -> Control {
        match received {
            Received::Suspend => Control::Pause,
            Received::Continue => Control::Resume,
            Received::Interrupt | Received::Terminate => {
                if std::mem::replace(&mut self.cancelled, true) {
                    return Control::ForceStop;
                }
                Control::Cancel(if received == Received::Interrupt {
                    Reason::Interrupted
                } else {
                    Reason::Terminated
                })
            }
        }
    }
}

pub fn apply(scope: &Scope, notices: Option<&Stopping>, control: Control) {
    match control {
        Control::Cancel(reason) => scope.cancel(reason),
        Control::ForceStop => {
            if let Some(Stopping {
                then: Then::Stop { forced, help },
                ..
            }) = notices
            {
                mix_ui::report(
                    mix_ui::Severity::Error,
                    &mix_ui::Report::new(forced).help(help),
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

pub fn listen(signal_number: Signal) -> tokio::signal::unix::Signal {
    signal(SignalKind::from_raw(signal_number as i32))
        .unwrap_or_else(|error| panic!("registering a {signal_number} handler: {error}"))
}

pub struct Terminal {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    suspend: tokio::signal::unix::Signal,
    resume: tokio::signal::unix::Signal,
}

impl Terminal {
    pub fn listen() -> Self {
        Self {
            interrupt: listen(Signal::SIGINT),
            terminate: listen(Signal::SIGTERM),
            suspend: listen(Signal::SIGTSTP),
            resume: listen(Signal::SIGCONT),
        }
    }

    pub async fn next(&mut self) -> Received {
        tokio::select! {
            _ = self.interrupt.recv() => Received::Interrupt,
            _ = self.terminate.recv() => Received::Terminate,
            _ = self.suspend.recv() => Received::Suspend,
            _ = self.resume.recv() => Received::Continue,
        }
    }
}

pub fn watch(scope: &Scope, notices: Option<Stopping>) -> Watch {
    let mut terminal = Terminal::listen();
    let scope = scope.clone();
    Watch(tokio::spawn(async move {
        let mut translator = Translator::default();
        loop {
            let control = translator.translate(terminal.next().await);
            apply(&scope, notices.as_ref(), control);
            if control == Control::Pause {
                let _ = nix::sys::signal::raise(Signal::SIGSTOP);
            }
        }
    }))
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
    use std::time::Duration;

    use nix::sys::signal;

    use super::*;

    static ONE_WATCH_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn translated(received: &[Received]) -> Vec<Control> {
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
            [Control::Cancel(Reason::Interrupted), Control::ForceStop]
        );
        assert_eq!(
            translated(&[Received::Terminate, Received::Interrupt]),
            [Control::Cancel(Reason::Terminated), Control::ForceStop]
        );
    }

    #[test]
    fn suspending_and_resuming_pause_and_resume_the_request() {
        assert_eq!(
            translated(&[Received::Suspend, Received::Continue]),
            [Control::Pause, Control::Resume]
        );
    }

    #[test]
    fn suspending_does_not_count_as_an_interrupt() {
        assert_eq!(
            translated(&[Received::Suspend, Received::Interrupt]),
            [Control::Pause, Control::Cancel(Reason::Interrupted)]
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

    #[test]
    fn only_a_run_that_can_detach_offers_to_leave_it_running() {
        assert_eq!(
            bootstrap().first_help().as_str(),
            "press Ctrl-C again to stop now"
        );
        assert_eq!(
            bootstrap().detached().first_help().as_str(),
            "press Ctrl-C again to leave it running in the background"
        );
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
        let _watch = watch(&scope, Some(bootstrap()));

        signal::raise(Signal::SIGINT).unwrap();

        cancelled_within_a_few_seconds(&scope).await;
    }

    #[tokio::test]
    async fn sigterm_cancels_the_request() {
        let _alone = ONE_WATCH_AT_A_TIME.lock().await;
        let scope = mix_exec::Scope::root();
        let _watch = watch(&scope, Some(bootstrap()));

        signal::raise(Signal::SIGTERM).unwrap();

        cancelled_within_a_few_seconds(&scope).await;
        assert_eq!(scope.reason(), Some(Reason::Terminated));
    }

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

        cancelled_within_a_few_seconds(&scope).await;
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
