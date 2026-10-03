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

pub async fn serve() -> ExitCode {
    let listener = match passed_by_systemd() {
        Ok(listener) => listener,
        Err(reason) => return stopped(reason),
    };
    let mut terminate = controls::listen(Signal::SIGTERM);
    let host = Host::new(Gate::Members);
    match mix_shell::request::recover(&host.locks, &mix_exec::Scope::root()).await {
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
        Err(error) => return stopped(error.to_string()),
    }
    let mut connections = tokio::task::JoinSet::new();
    let ended = loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(mix_rpc::serve_connection(host.clone(), stream));
                }
                Err(error) => break Some(format!("accepting a connection failed: {error}")),
            },
            _ = terminate.recv() => break None,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    };
    drop(listener);
    while connections.join_next().await.is_some() {}
    match ended {
        None => ExitCode::SUCCESS,
        Some(reason) => stopped(reason),
    }
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
