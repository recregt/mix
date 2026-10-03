//! State messages to systemd for `Type=notify`.
//!
//! Note: Without `NOTIFY_SOCKET`, as in `serve-stdin` and in tests, every message is dropped.

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

fn address(socket: &str) -> std::io::Result<SocketAddr> {
    match socket.strip_prefix('@') {
        Some(name) => SocketAddr::from_abstract_name(name.as_bytes()),
        None => SocketAddr::from_pathname(socket),
    }
}

fn send(message: &str) {
    let Some(socket) = crate::env::notify_socket() else {
        return;
    };
    let _ = address(&socket)
        .and_then(|address| UnixDatagram::unbound()?.send_to_addr(message.as_bytes(), &address));
}

pub fn ready(status: &str) {
    send(&format!("READY=1\nSTATUS={status}"));
}

pub fn status(status: &str) {
    send(&format!("STATUS={status}"));
}

pub fn stopping(status: &str) {
    send(&format!("STOPPING=1\nSTATUS={status}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_with_an_at_sign_is_abstract_and_any_other_is_a_path() {
        assert_eq!(
            address("@/org/freedesktop/systemd1/notify")
                .unwrap()
                .as_abstract_name(),
            Some(&b"/org/freedesktop/systemd1/notify"[..])
        );
        assert_eq!(
            address("/run/systemd/notify").unwrap().as_pathname(),
            Some(std::path::Path::new("/run/systemd/notify"))
        );
    }
}
