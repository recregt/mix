//! The audit behind `mix doctor`: every declared target inspected, once each.
//!
//! What was measured travels as a [`Finding`](crate::target::Finding) — the mode that was read
//! and the mode that was wanted, the ids an account carries, the unit file that is not there.
//! The measuring itself belongs to [`crate::target`], which `mix repair` reconciles from, so the
//! two commands cannot drift apart. The words are `mix-cli`'s.

use std::path::Path;
use std::sync::Arc;

use mix_core::action::Failure;
use mix_core::health::{self, wire};
use mix_core::models::Category;
use mix_events::v1::{
    Command, DoctorRequest, DoctorResult, Inspection, InspectionReport, InspectionResult, command,
    node_finished, node_started,
};
use mix_events::{Ending, ROOT, Start, Tree};

use crate::Context;
use crate::drive::{Observer, Performer, stopped_by};
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
    let items = mix_core::models::targets(ctx.user.as_ref(), &ctx.policy);
    let mut observer = ctx.relay();
    let mut tree = Tree::new(
        Arc::clone(&ctx.request.outbox),
        stopped_by(&ctx.scope),
        Start::command(
            "doctor",
            Command {
                mix_version: env!("CARGO_PKG_VERSION").to_string(),
                schema_minor: mix_events::SCHEMA_MINOR,
                request: Some(command::Request::Doctor(DoctorRequest {})),
            },
        )
        .planned(items.iter().map(|target| target.label().into_owned())),
    );
    let mut performer = Files::open(Path::new("/"), "audit").map(Performer::new);
    let mut reports = Vec::with_capacity(items.len());
    for target in &items {
        let finding = match &mut performer {
            Ok(performer) => match performer.observe(&health::queries(target)).await {
                Ok(facts) => health::classify(target, &facts),
                Err(failure) => Some(Finding::Unreadable {
                    kind: kind_of(&failure),
                }),
            },
            Err(error) => Some(Finding::Unreadable { kind: error.kind() }),
        };
        let name = target.label().into_owned();
        if let Ok(node) = tree.start(
            ROOT,
            Start::new(
                name.clone(),
                node_started::Kind::Inspection(Inspection {
                    target: name.clone(),
                    category: wire::category(target.category()) as i32,
                }),
            ),
        ) {
            let _ = tree.finish(
                node,
                Ending::succeeded().with_result(node_finished::Result::Inspection(
                    InspectionResult {
                        finding: finding.map(wire::finding),
                    },
                )),
            );
        }
        reports.push(HealthReport {
            name,
            category: target.category(),
            finding,
        });
    }
    let result = DoctorResult {
        reports: reports
            .iter()
            .map(|report| InspectionReport {
                target: report.name.clone(),
                category: wire::category(report.category) as i32,
                finding: report.finding.map(wire::finding),
            })
            .collect(),
    };
    let _ = tree.finish(
        ROOT,
        Ending::succeeded()
            .with_result(node_finished::Result::Doctor(result))
            .for_root(!reports.iter().all(HealthReport::healthy)),
    );
    drop(tree);
    observer.flush();
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

    struct Recorded(std::sync::Arc<std::sync::Mutex<Vec<mix_events::v1::Envelope>>>);

    impl crate::render::Render for Recorded {
        fn envelope(&mut self, envelope: mix_events::v1::Envelope) {
            self.0.lock().unwrap().push(envelope);
        }

        fn detail(&self) -> mix_events::Detail {
            mix_events::Detail::Trace
        }
    }

    #[tokio::test]
    async fn the_audit_is_one_valid_tree_with_a_node_per_target() {
        let cfg = user_config();
        let recorded = std::sync::Arc::default();
        let ctx = Context::new(mix_exec::Scope::root())
            .with_user(Some(cfg.clone()))
            .with_render(Recorded(std::sync::Arc::clone(&recorded)));

        let reports = audit(&ctx).await;

        let envelopes = recorded.lock().unwrap();
        assert!(mix_events::validate(envelopes.iter()).is_ok());
        let inspected = envelopes
            .iter()
            .filter(|envelope| {
                matches!(
                    &envelope.event,
                    Some(mix_events::v1::envelope::Event::NodeStarted(started))
                        if matches!(started.kind, Some(node_started::Kind::Inspection(_)))
                )
            })
            .count();
        assert_eq!(inspected, reports.len());
    }
}
