use std::os::fd::{FromRawFd, OwnedFd};
use std::process::ExitCode;

use nix::sys::signal::Signal;

use crate::controls;
use crate::worker::{Gate, Host};

const FIRST_PASSED_FD: i32 = 3;

fn passed_by_systemd() -> Result<tokio::net::UnixListener, String> {
    let ours = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
        == Some(std::process::id());
    if !ours || std::env::var("LISTEN_FDS").as_deref() != Ok("1") {
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
    let mut connections = tokio::task::JoinSet::new();
    let ended = loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(mix_rpc::serve_connection(
                        Host { gate: Gate::Members },
                        stream,
                    ));
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
