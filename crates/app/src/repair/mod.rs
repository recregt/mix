//! The reconciliation behind `mix repair`: every declared target measured, and the ones that
//! drifted put back.
//!
//! What can be measured and what can be put back are [`crate::target`]'s, not this command's:
//! `mix repair` decides the order it walks the environment in, what a change means for the
//! nix-daemon and for the git-tracked state, and what it hands the caller to print.

use mix_core::CancellationToken;
use mix_core::models::{Target, UserConfig, targets};
use mix_core::paths::{NIX_CONF_DEST, NIX_DAEMON_SERVICE_UNIT, mix_state_dir};

use crate::git;
use crate::systemd;
use crate::target::{self, Error};

pub struct RepairReport {
    pub name: String,
    pub fixed: bool,
    /// What stopped the repair, handed over rather than rendered: the caller decides how it
    /// should read, and a typed error is still there to be matched on.
    pub error: Option<Error>,
}

impl RepairReport {
    fn repaired(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fixed: true,
            error: None,
        }
    }

    fn failed(name: impl Into<String>, error: impl Into<Error>) -> Self {
        Self {
            name: name.into(),
            fixed: false,
            error: Some(error.into()),
        }
    }
}

pub struct Repair {
    pub reports: Vec<RepairReport>,
    pub interrupted: bool,
}

pub async fn repair(user_config: Option<&UserConfig>, cancel: &CancellationToken) -> Repair {
    tracing::info!("repairing managed environment");
    let items = targets(user_config);
    let (mut reports, interrupted) = put_back(&items, cancel).await;

    let token = CancellationToken::new();
    if rewrote_nix_conf(&reports) {
        restart_the_daemon(&token, &mut reports).await;
    }

    if let Some(cfg) = user_config {
        commit_the_tracked_state(cfg, &token, &mut reports).await;
    }

    Repair {
        reports,
        interrupted,
    }
}

async fn put_back(items: &[Target], cancel: &CancellationToken) -> (Vec<RepairReport>, bool) {
    let token = CancellationToken::new();

    // Measuring does not change anything, so every target is measured at once; putting them back
    // is done in the order they are declared in, because a target can be what the next one needs.
    let findings = futures_util::future::join_all(items.iter().map(target::inspect)).await;

    let mut reports = Vec::new();
    for (item, finding) in items.iter().zip(findings) {
        let Some(finding) = finding else { continue };
        let name = item.label();
        if cancel.is_cancelled() {
            tracing::info!("stopped before repairing {name}");
            return (reports, true);
        }
        tracing::debug!("drifted: {name}: {finding:?}");

        if let Some(reason) = finding.unfixable() {
            let artifact = name.into_owned();
            reports.push(RepairReport::failed(
                artifact.clone(),
                Error::Unrepairable { artifact, reason },
            ));
            continue;
        }

        match target::reconcile(item, finding, &token).await {
            Ok(()) => {
                tracing::debug!("repaired: {name}");
                reports.push(RepairReport::repaired(name.into_owned()));
            }
            Err(e) => {
                tracing::debug!("failed to repair {name}: {e}");
                reports.push(RepairReport::failed(name.into_owned(), e));
            }
        }
    }

    (reports, false)
}

/// A rewritten nix.conf only reaches a running nix-daemon when it restarts.
fn rewrote_nix_conf(reports: &[RepairReport]) -> bool {
    reports
        .iter()
        .any(|report| report.fixed && report.name == NIX_CONF_DEST)
}

async fn restart_the_daemon(token: &CancellationToken, reports: &mut Vec<RepairReport>) {
    match systemd::restart_if_active(NIX_DAEMON_SERVICE_UNIT, token).await {
        Ok(false) => {}
        Ok(true) => reports.push(RepairReport::repaired(NIX_DAEMON_SERVICE_UNIT)),
        Err(e) => reports.push(RepairReport::failed(NIX_DAEMON_SERVICE_UNIT, e)),
    }
}

/// Configuration mix rewrote is drift the user should be able to see in git.
async fn commit_the_tracked_state(
    cfg: &UserConfig,
    token: &CancellationToken,
    reports: &mut Vec<RepairReport>,
) {
    const NAME: &str = "git-tracked state";

    let state_dir = mix_state_dir(&cfg.user.home);
    let git = git::Git::resolve(&cfg.user).await;
    match git.sync(&cfg.user, &state_dir, token).await {
        Ok(true) => {
            tracing::debug!("committed drift in git-tracked state");
            reports.push(RepairReport::repaired(NAME));
        }
        Ok(false) => {}
        Err(e) => {
            tracing::debug!("failed to commit git-tracked state: {e}");
            reports.push(RepairReport::failed(NAME, e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(name: &str, fixed: bool) -> RepairReport {
        RepairReport {
            name: name.to_string(),
            fixed,
            error: None,
        }
    }

    #[test]
    fn a_repaired_nix_conf_asks_for_a_daemon_restart() {
        assert!(rewrote_nix_conf(&[report(NIX_CONF_DEST, true)]));
    }

    #[test]
    fn a_healthy_nix_conf_leaves_the_daemon_alone() {
        assert!(!rewrote_nix_conf(&[report("/nix", true)]));
        assert!(!rewrote_nix_conf(&[report(NIX_CONF_DEST, false)]));
        assert!(!rewrote_nix_conf(&[]));
    }

    #[test]
    fn a_report_carries_either_a_repair_or_the_reason_there_was_none() {
        let repaired = RepairReport::repaired("/nix");
        assert!(repaired.fixed && repaired.error.is_none());

        let failed = RepairReport::failed(
            "/nix",
            Error::Unrepairable {
                artifact: "/nix".to_string(),
                reason: crate::target::Unfixable::NotADirectory,
            },
        );
        assert!(!failed.fixed && failed.error.is_some());
    }

    fn drifted_files(dir: &std::path::Path) -> Vec<Target> {
        ["one", "two"]
            .into_iter()
            .map(|name| {
                let path = dir.join(name);
                std::fs::write(&path, "drifted").unwrap();
                Target::File {
                    path,
                    expected: Some("expected".to_string()),
                    owner: None,
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn every_drifted_target_is_put_back_when_nothing_stops_it() {
        let dir = tempfile::tempdir().unwrap();

        let (reports, interrupted) =
            put_back(&drifted_files(dir.path()), &CancellationToken::new()).await;

        assert!(!interrupted);
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(|report| report.fixed));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("two")).unwrap(),
            "expected"
        );
    }

    #[tokio::test]
    async fn a_stopped_repair_starts_nothing_new_and_says_it_was_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let (reports, interrupted) = put_back(&drifted_files(dir.path()), &cancel).await;

        assert!(interrupted);
        assert!(reports.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("one")).unwrap(),
            "drifted"
        );
    }

    #[tokio::test]
    async fn a_stop_with_nothing_left_to_repair_is_not_an_interruption() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let healthy = Target::File {
            path: dir.path().join("healthy"),
            expected: Some("expected".to_string()),
            owner: None,
        };
        std::fs::write(dir.path().join("healthy"), "expected").unwrap();

        let (reports, interrupted) = put_back(&[healthy], &cancel).await;

        assert!(!interrupted);
        assert!(reports.is_empty());
    }
}
