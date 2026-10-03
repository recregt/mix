use mix_exec::Reason;
use mix_ui::{Help, Note, help, note};
use nix::sys::signal::Signal;
use tokio::signal::unix::{SignalKind, signal};

pub fn second_ctrl_c() -> Help {
    help!("press Ctrl-C again to leave it running in the background")
}

pub fn detached() -> Note {
    note!("`mix` is finishing the cleanup in the background")
}

pub const DETACHED_EXIT: u32 = mix_events::exit::INTERRUPTED;

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
    Detach,
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
                    return Control::Detach;
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

fn listen(signal_number: Signal) -> tokio::signal::unix::Signal {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn translated(received: &[Received]) -> Vec<Control> {
        let mut translator = Translator::default();
        received
            .iter()
            .map(|received| translator.translate(*received))
            .collect()
    }

    #[test]
    fn a_first_interrupt_cancels_and_a_second_detaches() {
        assert_eq!(
            translated(&[Received::Interrupt, Received::Interrupt]),
            [Control::Cancel(Reason::Interrupted), Control::Detach]
        );
        assert_eq!(
            translated(&[Received::Terminate, Received::Interrupt]),
            [Control::Cancel(Reason::Terminated), Control::Detach]
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
}
