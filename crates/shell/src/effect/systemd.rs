//! Asking systemd about a unit, and asking it to start, restart or re-read one.

use crate::effect::exec::run;
use mix_core::{Result, Scope};

/// Restarts a unit that is running, and says whether it had to be restarted.
pub async fn restart_if_active(name: &str, scope: &Scope) -> Result<bool> {
    if !unit_is_active(name, scope).await {
        return Ok(false);
    }
    run("systemctl", &["restart", name], scope).await?;
    Ok(true)
}

pub async fn unit_is_active(name: &str, scope: &Scope) -> bool {
    tracing::debug!("checking systemd unit is-active: {name}");
    mix_exec::Command::new("systemctl")
        .args(["is-active", "--quiet", name])
        .output(scope)
        .await
        .is_ok_and(|output| output.status.success())
}

/// Re-reads the unit files on disk, after one of them was replaced.
pub async fn daemon_reload(scope: &Scope) -> Result<()> {
    run("systemctl", &["daemon-reload"], scope).await
}

/// Starts a unit now and on every boot.
pub async fn enable_now(name: &str, scope: &Scope) -> Result<()> {
    run("systemctl", &["enable", "--now", name], scope).await
}
