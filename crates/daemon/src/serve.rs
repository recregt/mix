use std::collections::HashSet;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};

use mix_rpc::{Caller, Worker};
use nix::errno::Errno;
use nix::sys::signal::Signal;

use crate::controls;
use crate::worker::{Gate, Host};

const FIRST_PASSED_FD: i32 = 3;

#[derive(Clone, Default)]
struct Outsiders(Arc<Mutex<HashSet<u32>>>);

struct Outsider {
    uid: u32,
    outsiders: Outsiders,
}

impl Outsiders {
    fn admit(&self, uid: u32) -> Option<Outsider> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(uid)
            .then(|| Outsider {
                uid,
                outsiders: self.clone(),
            })
    }
}

impl Drop for Outsider {
    fn drop(&mut self) {
        self.outsiders
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.uid);
    }
}

fn admission(
    host: &Host,
    outsiders: &Outsiders,
    stream: &tokio::net::UnixStream,
) -> Option<Option<Outsider>> {
    let credentials = stream.peer_cred().ok()?;
    let caller = Caller {
        uid: credentials.uid(),
        gid: credentials.gid(),
    };
    if host.admits(caller) {
        Some(None)
    } else {
        outsiders.admit(caller.uid).map(Some)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AfterAcceptError {
    Retry,
    WaitForAClose,
    Stop,
}

fn after_accept_error(error: &std::io::Error, open: usize) -> AfterAcceptError {
    match error.raw_os_error().map(Errno::from_raw) {
        Some(Errno::ECONNABORTED | Errno::EINTR) => AfterAcceptError::Retry,
        Some(Errno::EMFILE | Errno::ENFILE | Errno::ENOBUFS | Errno::ENOMEM) if open > 0 => {
            AfterAcceptError::WaitForAClose
        }
        _ => AfterAcceptError::Stop,
    }
}

fn passed_by_systemd() -> Result<tokio::net::UnixListener, String> {
    let ours = crate::env::listen_pid() == Some(std::process::id());
    if !ours || crate::env::listen_fds().as_deref() != Some("1") {
        return Err("systemd passed no socket to this process".to_string());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(FIRST_PASSED_FD) };
    let listener = std::os::unix::net::UnixListener::from(fd);
    listener
        .local_addr()
        .and_then(|_| listener.set_nonblocking(true))
        .and_then(|()| tokio::net::UnixListener::from_std(listener))
        .map_err(|error| format!("the socket systemd passed is not a Unix socket: {error}"))
}

fn stopped(reason: String) -> ExitCode {
    mix_ui::report(
        mix_ui::Severity::Error,
        &mix_ui::Report::new(&mix_ui::phrase!("the daemon stopped serving")).causes(vec![reason]),
    );
    ExitCode::FAILURE
}

fn serving(count: usize, waiting: usize) -> String {
    let serving = match count {
        0 => "Waiting for requests".to_string(),
        1 => "Serving 1 request".to_string(),
        count => format!("Serving {count} requests"),
    };
    match waiting {
        0 => serving,
        waiting => format!("{serving}, {waiting} waiting for a lock"),
    }
}

pub async fn serve() -> ExitCode {
    let listener = match passed_by_systemd() {
        Ok(listener) => listener,
        Err(reason) => return stopped(reason),
    };
    let mut terminate = controls::listen(Signal::SIGTERM);
    let mut drain = controls::listen(mix_shell::effect::units::DRAIN);
    crate::notify::ready("Recovering interrupted requests");
    let host = Host::new(Gate::Members);
    let scope = mix_exec::Scope::root();
    let recovery = mix_shell::request::recover(&host.locks, &scope);
    tokio::pin!(recovery);
    let (recovered, stopping) = tokio::select! {
        recovered = &mut recovery => (recovered, false),
        _ = terminate.recv() => {
            scope.cancel(mix_exec::Reason::Terminated);
            ((&mut recovery).await, true)
        }
    };
    match recovered {
        Ok(recovered) => {
            for (action, failure) in recovered.failures {
                mix_ui::report(
                    mix_ui::Severity::Warning,
                    &mix_ui::Report::new(&mix_ui::phrase!(
                        "could not finish an interrupted request"
                    ))
                    .causes(vec![format!("{action:?}: {failure:?}")]),
                );
            }
        }
        Err(_) if stopping => return ExitCode::SUCCESS,
        Err(error) => return stopped(error.to_string()),
    }
    if stopping {
        return ExitCode::SUCCESS;
    }
    let mut waiting = host.locks.waiting();
    crate::notify::status(&serving(0, 0));
    host.installed
        .warm_enrolled(mix_shell::request::stored_mirror());
    let mut connections = tokio::task::JoinSet::new();
    let outsiders = Outsiders::default();
    let mut full = false;
    let ended = loop {
        tokio::select! {
            accepted = listener.accept(), if !full => match accepted {
                Ok((stream, _)) => {
                    let Some(admitted) = admission(&host, &outsiders, &stream) else {
                        continue;
                    };
                    let host = host.clone();
                    connections.spawn(async move {
                        let served = mix_rpc::serve_connection(host, stream).await;
                        drop(admitted);
                        served
                    });
                    crate::notify::status(&serving(connections.len(), *waiting.borrow()));
                }
                Err(error) => match after_accept_error(&error, connections.len()) {
                    AfterAcceptError::Retry => {}
                    AfterAcceptError::WaitForAClose => full = true,
                    AfterAcceptError::Stop => {
                        break Ended::Failed(format!("accepting a connection failed: {error}"));
                    }
                },
            },
            _ = terminate.recv() => break Ended::Stopped,
            _ = drain.recv() => break Ended::Drained,
            Ok(()) = waiting.changed() => {
                crate::notify::status(&serving(connections.len(), *waiting.borrow()));
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {
                full = false;
                crate::notify::status(&serving(connections.len(), *waiting.borrow()));
            }
        }
    };
    drop(listener);
    if matches!(ended, Ended::Drained) {
        crate::notify::stopping(&format!(
            "Finishing {} before restarting",
            match connections.len() {
                1 => "1 request".to_string(),
                count => format!("{count} requests"),
            }
        ));
    }
    while connections.join_next().await.is_some() {}
    match ended {
        Ended::Stopped => ExitCode::SUCCESS,
        Ended::Drained => ExitCode::from(mix_core::declared::targets::MIX_DAEMON_DRAINED),
        Ended::Failed(reason) => stopped(reason),
    }
}

enum Ended {
    Stopped,
    Drained,
    Failed(String),
}

#[cfg(test)]
mod tests {
    use nix::errno::Errno;

    use super::{AfterAcceptError, Outsiders, after_accept_error, serving};

    #[test]
    fn running_out_of_descriptors_waits_for_a_connection_to_close() {
        let error = |errno: Errno| std::io::Error::from_raw_os_error(errno as i32);
        assert_eq!(
            after_accept_error(&error(Errno::EMFILE), 3),
            AfterAcceptError::WaitForAClose
        );
        assert_eq!(
            after_accept_error(&error(Errno::ENFILE), 1),
            AfterAcceptError::WaitForAClose
        );
        assert_eq!(
            after_accept_error(&error(Errno::EMFILE), 0),
            AfterAcceptError::Stop
        );
        assert_eq!(
            after_accept_error(&error(Errno::ECONNABORTED), 0),
            AfterAcceptError::Retry
        );
        assert_eq!(
            after_accept_error(&error(Errno::EBADF), 3),
            AfterAcceptError::Stop
        );
    }

    #[test]
    fn a_user_outside_mix_users_holds_one_connection_at_a_time() {
        let outsiders = Outsiders::default();
        let first = outsiders.admit(1000);
        assert!(first.is_some());
        assert!(outsiders.admit(1000).is_none());
        assert!(outsiders.admit(1001).is_some());
        drop(first);
        assert!(outsiders.admit(1000).is_some());
    }

    #[test]
    fn the_status_counts_the_requests_served_and_the_ones_waiting_for_a_lock() {
        assert_eq!(serving(0, 0), "Waiting for requests");
        assert_eq!(serving(1, 0), "Serving 1 request");
        assert_eq!(serving(3, 2), "Serving 3 requests, 2 waiting for a lock");
    }

    #[test]
    fn the_client_looks_for_the_socket_unit_where_mix_installs_it() {
        assert_eq!(
            mix_rpc::SOCKET_UNIT_FILE,
            mix_core::declared::paths::MIX_DAEMON_SOCKET_DEST
        );
    }

    #[test]
    fn the_installed_socket_listens_where_the_client_dials() {
        assert!(
            mix_core::declared::targets::MIX_DAEMON_SOCKET
                .contains(&format!("ListenStream={}\n", mix_rpc::SOCKET_PATH))
        );
    }
}
