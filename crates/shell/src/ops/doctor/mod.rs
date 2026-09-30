//! The audit behind `mix doctor`: every declared target inspected, once each.
//!
//! What was measured travels as a [`Finding`](crate::target::Finding) — the mode that was read
//! and the mode that was wanted, the ids an account carries, the unit file that is not there.
//! The measuring itself belongs to [`crate::target`], which `mix repair` reconciles from, so the
//! two commands cannot drift apart. The words are `mix-cli`'s.

use std::path::Path;

use mix_core::action::Failure;
use mix_core::health;
use mix_core::models::Category;

use crate::Context;
use crate::drive::Performer;
use crate::effect::files::Files;
use crate::target::Finding;

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

pub async fn audit(ctx: &Context) -> Vec<HealthReport> {
    tracing::info!("auditing managed environment");
    let items = mix_core::models::targets(ctx.user.as_ref(), &ctx.policy);
    let mut performer = match Files::open(Path::new("/"), "audit") {
        Ok(files) => Some(Performer::new(files)),
        Err(error) => {
            tracing::debug!("cannot open the root to audit: {error}");
            None
        }
    };
    let mut reports = Vec::with_capacity(items.len());
    for target in &items {
        let finding = match &mut performer {
            Some(performer) => match performer.observe(&health::queries(target)).await {
                Ok(facts) => health::classify(target, &facts),
                Err(failure) => Some(Finding::Unreadable {
                    kind: kind_of(&failure),
                }),
            },
            None => Some(Finding::Unreadable {
                kind: std::io::ErrorKind::PermissionDenied,
            }),
        };
        let name = target.label().into_owned();
        if let Some(finding) = &finding {
            tracing::debug!("unhealthy: {name}: {finding:?}");
        }
        reports.push(HealthReport {
            name,
            category: target.category(),
            finding,
        });
    }
    reports
}

fn kind_of(failure: &Failure) -> std::io::ErrorKind {
    match failure {
        Failure::Io { kind, .. } | Failure::SpawnFailed { kind, .. } => *kind,
        _ => std::io::ErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mix_core::privilege::InvokingUser;

    use super::*;

    fn user_config() -> mix_core::models::UserConfig {
        mix_core::models::UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: PathBuf::from("/home/mix-user"),
            },
            flake: "flake-content".to_string(),
            lock: "lock-content".to_string(),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    #[tokio::test]
    async fn audit_reports_one_entry_per_target() {
        let reports = audit(&Context::new(mix_exec::Scope::root())).await;
        assert_eq!(
            reports.len(),
            mix_core::models::targets(None, &mix_core::policy::Policy::default()).len()
        );
    }

    #[tokio::test]
    async fn audit_covers_the_per_user_targets_of_the_config_it_is_given() {
        let cfg = user_config();

        let reports =
            audit(&Context::new(mix_exec::Scope::root()).with_user(Some(cfg.clone()))).await;

        assert_eq!(
            reports.len(),
            mix_core::models::targets(Some(&cfg), &mix_core::policy::Policy::default()).len(),
            "every target of the injected config must be reported"
        );
        assert!(reports.len() > audit(&Context::new(mix_exec::Scope::root())).await.len());
        assert!(
            reports
                .iter()
                .any(|report| report.name.contains("/home/mix-user"))
        );
    }

    #[tokio::test]
    async fn a_report_is_healthy_exactly_when_nothing_was_found() {
        let cfg = user_config();

        for report in
            audit(&Context::new(mix_exec::Scope::root()).with_user(Some(cfg.clone()))).await
        {
            assert_eq!(report.healthy(), report.finding.is_none());
        }
    }
}
