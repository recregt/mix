//! Environment variable configuration for the `mix-daemon` service.

/// Process ID that systemd passed its listening sockets to. The passed socket is used only
/// when it matches this process.
pub const LISTEN_PID: &str = "LISTEN_PID";

/// Number of listening sockets passed by systemd. `mix-daemon serve` expects exactly one.
pub const LISTEN_FDS: &str = "LISTEN_FDS";

/// User ID of the account that ran `sudo mix-daemon serve-stdin`, set by `sudo`. Resolves the
/// invoking user for a root caller.
pub const SUDO_UID: &str = "SUDO_UID";

#[allow(clippy::disallowed_methods)]
fn read(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub fn listen_pid() -> Option<u32> {
    read(LISTEN_PID)?.parse().ok()
}

pub fn listen_fds() -> Option<String> {
    read(LISTEN_FDS)
}

pub fn sudo_uid() -> Option<String> {
    read(SUDO_UID)
}
