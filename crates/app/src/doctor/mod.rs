//! The audit behind `mix doctor`: every declared target inspected, once each.
//!
//! What was measured travels as a [`Finding`](crate::target::Finding) — the mode that was read
//! and the mode that was wanted, the ids an account carries, the unit file that is not there.
//! The measuring itself belongs to [`crate::target`], which `mix repair` reconciles from, so the
//! two commands cannot drift apart. The words are `mix-cli`'s.

use futures_util::future::join_all;
use mix_core::models::{Category, UserConfig};

use crate::target::{self, Finding};

pub struct HealthReport {
    pub name: String,
    pub category: Category,
    /// What the inspection measured, or `None` when the artifact is as it should be.
    pub finding: Option<Finding>,
}

impl HealthReport {
    pub fn healthy(&self) -> bool {
        self.finding.is_none()
    }
}

pub async fn audit(user_config: Option<&UserConfig>) -> Vec<HealthReport> {
    tracing::info!("auditing managed environment");
    let items = mix_core::models::targets(user_config);
    let findings = join_all(items.iter().map(target::inspect)).await;
    items
        .iter()
        .zip(findings)
        .map(|(target, finding)| {
            let name = target.label().into_owned();
            if let Some(finding) = &finding {
                tracing::debug!("unhealthy: {name}: {finding:?}");
            }
            HealthReport {
                name,
                category: target.category(),
                finding,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mix_core::privilege::InvokingUser;

    use super::*;

    fn user_config() -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: PathBuf::from("/home/mix-user"),
            },
            flake: "flake-content".to_string(),
            home: "home-content".to_string(),
        }
    }

    #[tokio::test]
    async fn audit_reports_one_entry_per_target() {
        let reports = audit(None).await;
        assert_eq!(reports.len(), mix_core::models::targets(None).len());
    }

    #[tokio::test]
    async fn audit_covers_the_per_user_targets_of_the_config_it_is_given() {
        let cfg = user_config();

        let reports = audit(Some(&cfg)).await;

        assert_eq!(
            reports.len(),
            mix_core::models::targets(Some(&cfg)).len(),
            "every target of the injected config must be reported"
        );
        assert!(reports.len() > audit(None).await.len());
        assert!(
            reports
                .iter()
                .any(|report| report.name.contains("/home/mix-user"))
        );
    }

    #[tokio::test]
    async fn a_report_is_healthy_exactly_when_nothing_was_found() {
        let cfg = user_config();

        for report in audit(Some(&cfg)).await {
            assert_eq!(report.healthy(), report.finding.is_none());
        }
    }
}
