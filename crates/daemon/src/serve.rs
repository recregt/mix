use std::os::fd::{FromRawFd, OwnedFd};
use std::process::ExitCode;

use nix::sys::signal::Signal;

use crate::controls;
use crate::worker::{Gate, Host};

const FIRST_PASSED_FD: i32 = 3;

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

fn serving(count: usize) -> String {
    match count {
        0 => "Waiting for requests".to_string(),
        1 => "Serving 1 request".to_string(),
        count => format!("Serving {count} requests"),
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
    crate::notify::status(&serving(0));
    let mut connections = tokio::task::JoinSet::new();
    let ended = loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(mix_rpc::serve_connection(host.clone(), stream));
                    crate::notify::status(&serving(connections.len()));
                }
                Err(error) => break Ended::Failed(format!("accepting a connection failed: {error}")),
            },
            _ = terminate.recv() => break Ended::Stopped,
            _ = drain.recv() => break Ended::Drained,
            Some(_) = connections.join_next(), if !connections.is_empty() => {
                crate::notify::status(&serving(connections.len()));
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
        Ended::Drained => ExitCode::from(mix_core::targets::MIX_DAEMON_DRAINED),
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
    #[test]
    fn the_installed_socket_listens_where_the_client_dials() {
        assert!(
            mix_core::targets::MIX_DAEMON_SOCKET
                .contains(&format!("ListenStream={}\n", mix_rpc::SOCKET_PATH))
        );
    }
}
