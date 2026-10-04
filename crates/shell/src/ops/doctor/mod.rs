//! `mix doctor` checks every declared target, the system's and the caller's, against what
//! it should be, and reports what it finds. `mix repair` uses the same checks to fix those
//! targets. Because both rely on the same logic, detection and repair never go out of sync.

use std::path::Path;

use mix_core::action::Failure;
use mix_core::health::{self, wire};
use mix_events::v1::{
    DoctorRequest, DoctorResult, Inspection, InspectionResult, Plan, node_finished, node_started,
};
use mix_events::{Ending, ROOT, Start};

use crate::Context;
use crate::drive::Performer;
use crate::effect::files::Files;
use crate::request::{Concluded, Root};
use crate::target::Finding;

use mix_core::health::HealthReport;

pub(crate) async fn audit(ctx: &Context, root: &mut Root, _request: &DoctorRequest) -> Concluded {
    let items = mix_core::targets::targets(ctx.user.as_ref(), &ctx.policy);
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
            Ok(performer) => match performer
                .observe(&health::queries(target), &ctx.scope)
                .await
            {
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
                        finding: finding.clone().map(wire::finding),
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
        reports: reports.iter().map(wire::report).collect(),
    };
    let _ = tree.finish(plan, Ending::succeeded());
    let healthy = reports.iter().all(HealthReport::healthy);
    root.conclude_with_problems(
        Ending::succeeded().with_result(node_finished::Result::Doctor(result)),
        !healthy,
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

    use mix_core::identity::InvokingUser;
    use mix_events::v1::InspectionReport;
    use mix_events::v1::command::Request;

    use super::*;
    use crate::request::ran::{Ran, ran};

    fn user_config() -> mix_core::targets::UserConfig {
        mix_core::targets::UserConfig {
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

    async fn audited(session: crate::Session) -> (Ran, Vec<InspectionReport>) {
        let ran = ran(session, Request::Doctor(DoctorRequest {})).await;
        let reports = match ran.result() {
            Some(node_finished::Result::Doctor(doctor)) => doctor.reports.clone(),
            other => panic!("expected a doctor result, got {other:?}"),
        };
        (ran, reports)
    }

    #[tokio::test]
    async fn audit_reports_one_entry_per_target() {
        let (_, reports) = audited(session()).await;
        assert_eq!(
            reports.len(),
            mix_core::targets::targets(None, &mix_core::policy::Policy::default()).len()
        );
    }

    #[tokio::test]
    async fn audit_covers_the_per_user_targets_of_the_config_it_is_given() {
        let cfg = user_config();

        let (_, reports) = audited(session().with_user(Some(cfg.clone()))).await;

        assert_eq!(
            reports.len(),
            mix_core::targets::targets(Some(&cfg), &mix_core::policy::Policy::default()).len(),
            "every target of the injected config must be reported"
        );
        assert!(reports.len() > audited(session()).await.1.len());
        assert!(
            reports
                .iter()
                .any(|report| report.target.contains("/home/mix-user"))
        );
    }

    #[tokio::test]
    async fn the_audit_has_a_node_per_target() {
        let (ran, reports) = audited(session().with_user(Some(user_config()))).await;

        let inspected = ran
            .0
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
