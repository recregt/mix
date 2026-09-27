//! Asking systemd about a unit, and asking it to start, restart or re-read one.

use crate::exec::{command, format_command, output, run};
use mix_core::{CancellationToken, Result};

/// Restarts a unit that is running, and says whether it had to be restarted.
pub async fn restart_if_active(name: &str, token: &CancellationToken) -> Result<bool> {
    if !unit_is_active(name).await {
        return Ok(false);
    }
    run("systemctl", &["restart", name], token).await?;
    Ok(true)
}

pub async fn unit_is_active(name: &str) -> bool {
    tracing::debug!("checking systemd unit is-active: {name}");
    let args = ["is-active", "--quiet", name];
    let mut systemctl = command("systemctl");
    systemctl.args(args);
    output(
        systemctl,
        &format_command("systemctl", &args),
        None,
        &mix_core::cancel::shield(),
    )
    .await
    .is_ok_and(|output| output.status.success())
}

/// Re-reads the unit files on disk, after one of them was replaced.
pub async fn daemon_reload(token: &CancellationToken) -> Result<()> {
    run("systemctl", &["daemon-reload"], token).await
}

/// Starts a unit now and on every boot.
pub async fn enable_now(name: &str, token: &CancellationToken) -> Result<()> {
    run("systemctl", &["enable", "--now", name], token).await
}
