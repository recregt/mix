//! The audit behind `mix doctor`: every declared target inspected, once each.
//!
//! What was measured travels as a [`Finding`](crate::target::Finding) — the mode that was read
//! and the mode that was wanted, the ids an account carries, the unit file that is not there.
//! The measuring itself belongs to [`crate::target`], which `mix repair` reconciles from, so the
//! two commands cannot drift apart. The words are `mix-cli`'s.

use std::path::Path;

use mix_core::action::Failure;
use mix_core::health::{self, Drift, wire};
use mix_core::models::Category;
use mix_events::v1::{
    DoctorResult, Inspection, InspectionReport, InspectionResult, Plan, node_finished, node_started,
};
use mix_events::{Ending, ROOT, Start};

use crate::Context;
use crate::drive::Performer;
use crate::effect::files::Files;
use crate::request::{Concluded, Root};
use crate::target::Finding;

pub struct HealthReport {
    pub name: String,
    pub category: Category,
    pub finding: Option<Finding>,
    pub drift: Option<Drift>,
}

impl HealthReport {
    pub fn healthy(&self) -> bool {
        self.finding.is_none()
    }
}

pub(crate) async fn audit(ctx: &Context, root: &mut Root) -> Concluded<Vec<HealthReport>> {
    let items = mix_core::models::targets(ctx.user.as_ref(), &ctx.policy);
    let tree = &mut root.tree;
    let plan = tree
        .start(
            ROOT,
            Start::new("audit", node_started::Kind::Plan(Plan::default()))
                .planned(items.iter().map(|target| target.label().into_owned())),
        )
        .expect("the root is open");
    let mut performer = Files::open(Path::new("/"), "audit").map(Performer::new);
    let mut reports = Vec::with_capacity(items.len());
    for target in &items {
        let (finding, drift) = match &mut performer {
            Ok(performer) => match performer.observe(&health::queries(target)).await {
                Ok(facts) => (
                    health::classify(target, &facts),
                    health::drift(target, &facts),
                ),
                Err(failure) => (
                    Some(Finding::Unreadable {
                        kind: kind_of(&failure),
                    }),
                    None,
                ),
            },
            Err(error) => (Some(Finding::Unreadable { kind: error.kind() }), None),
        };
        let name = target.label().into_owned();
        if let Ok(node) = tree.start(
            plan,
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
                        drift: drift.as_ref().map(wire::drift),
                    },
                )),
            );
        }
        reports.push(HealthReport {
            name,
            category: target.category(),
            finding,
            drift,
        });
    }
    let result = DoctorResult {
        reports: reports
            .iter()
            .map(|report| InspectionReport {
                target: report.name.clone(),
                category: wire::category(report.category) as i32,
                finding: report.finding.map(wire::finding),
                drift: report.drift.as_ref().map(wire::drift),
            })
            .collect(),
    };
    let _ = tree.finish(plan, Ending::succeeded());
    let healthy = reports.iter().all(HealthReport::healthy);
    root.conclude_with_problems(
        Ending::succeeded().with_result(node_finished::Result::Doctor(result)),
        !healthy,
        reports,
    )
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

    fn session() -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_policy(mix_core::policy::Policy::default())
    }

    #[tokio::test]
    async fn audit_reports_one_entry_per_target() {
        let reports = crate::request::doctor(&session()).await;
        assert_eq!(
            reports.len(),
            mix_core::models::targets(None, &mix_core::policy::Policy::default()).len()
        );
    }

    #[tokio::test]
    async fn audit_covers_the_per_user_targets_of_the_config_it_is_given() {
        let cfg = user_config();

        let reports = crate::request::doctor(&session().with_user(Some(cfg.clone()))).await;

        assert_eq!(
            reports.len(),
            mix_core::models::targets(Some(&cfg), &mix_core::policy::Policy::default()).len(),
            "every target of the injected config must be reported"
        );
        assert!(reports.len() > crate::request::doctor(&session()).await.len());
        assert!(
            reports
                .iter()
                .any(|report| report.name.contains("/home/mix-user"))
        );
    }

    #[tokio::test]
    async fn a_report_is_healthy_exactly_when_nothing_was_found() {
        let cfg = user_config();

        for report in crate::request::doctor(&session().with_user(Some(cfg.clone()))).await {
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
        let ctx = session()
            .with_user(Some(cfg.clone()))
            .with_render(Recorded(std::sync::Arc::clone(&recorded)));

        let reports = crate::request::doctor(&ctx).await;

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
