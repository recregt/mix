//! `mix doctor` checks every declared target, the system's and the caller's, against what
//! it should be, and reports what it finds. `mix repair` uses the same checks to fix those
//! targets. Because both rely on the same logic, detection and repair never go out of sync.

use mix_core::effect::Failure;
use mix_core::ops::health;
use mix_core::report::inspection;
use mix_events::v1::{
    DoctorRequest, DoctorResult, Inspection, InspectionResult, Plan, node_finished, node_started,
};
use mix_events::{Ending, ROOT, Start};

use crate::Context;
use crate::request::{Concluded, Root};

use mix_core::ops::health::HealthReport;

pub(crate) async fn audit(ctx: &Context, root: &mut Root, _request: &DoctorRequest) -> Concluded {
    let mut audit = health::Audit::new(mix_core::declared::targets::tree(
        ctx.user.as_ref(),
        &ctx.policy,
    ));
    let tree = &mut root.tree;
    let plan = tree
        .start(
            ROOT,
            Start::new("audit", node_started::Kind::Plan(Plan::default())).planned(audit.planned()),
        )
        .expect("the root is open");
    let mut performer = ctx.performer();
    let mut facts = None;
    loop {
        match audit.step(facts.take()) {
            health::Audited::Observe(queries) => {
                facts = Some(match &mut performer {
                    Ok(performer) => performer.observe(&queries, &ctx.scope).await,
                    Err(error) => Err(Failure::Io {
                        path: "/".into(),
                        kind: error.kind(),
                    }),
                });
            }
            health::Audited::Reported => {
                if let Some(report) = audit.last() {
                    show(tree, plan, report);
                }
            }
            health::Audited::Done => break,
        }
    }
    let reports = audit.into_reports();
    let result = DoctorResult {
        reports: reports.iter().map(inspection::report).collect(),
    };
    let _ = tree.finish(plan, Ending::succeeded());
    let healthy = reports.iter().all(HealthReport::healthy);
    root.conclude_with_problems(
        Ending::succeeded().with_result(node_finished::Result::Doctor(result)),
        !healthy,
    )
}

fn show(tree: &mut mix_events::Tree, plan: mix_events::NodeId, report: &HealthReport) {
    if let Some(cause) = &report.blocked_by {
        let _ = tree.blocked(plan, report.name.clone(), cause.clone());
        return;
    }
    if let Ok(node) = tree.start(
        plan,
        Start::new(
            report.name.clone(),
            node_started::Kind::Inspection(Inspection {
                target: report.name.clone(),
                category: inspection::category(report.category) as i32,
            }),
        ),
    ) {
        let _ = tree.finish(
            node,
            Ending::succeeded().with_result(node_finished::Result::Inspection(InspectionResult {
                finding: report.finding.clone().map(inspection::finding),
                drift: report.drift.as_ref().map(inspection::drift),
            })),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mix_core::declared::identity::InvokingUser;
    use mix_events::v1::InspectionReport;
    use mix_events::v1::command::Request;

    use super::*;
    use crate::request::ran::{Ran, ran};

    fn user_config() -> mix_core::declared::targets::UserConfig {
        mix_core::declared::targets::UserConfig {
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
            .with_policy(mix_core::declared::policy::Policy::default())
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
            mix_core::declared::targets::tree(None, &mix_core::declared::policy::Policy::default())
                .len()
        );
    }

    #[tokio::test]
    async fn audit_covers_the_per_user_targets_of_the_config_it_is_given() {
        let cfg = user_config();

        let (_, reports) = audited(session().with_user(Some(cfg.clone()))).await;

        assert_eq!(
            reports.len(),
            mix_core::declared::targets::tree(
                Some(&cfg),
                &mix_core::declared::policy::Policy::default()
            )
            .len(),
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
    async fn the_audit_inspects_each_check_or_names_what_blocked_it() {
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
        let blocked: Vec<(&str, &str)> = ran
            .0
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Some(mix_events::v1::envelope::Event::NotRun(not_run))
                    if not_run.reason() == mix_events::v1::NotRunReason::Blocked =>
                {
                    Some((not_run.key.as_str(), not_run.blocked_by.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(inspected + blocked.len(), reports.len());
        for (key, by) in blocked {
            let report = reports.iter().find(|report| report.target == key).unwrap();
            assert_eq!(report.blocked_by, by);
            let cause = reports.iter().find(|report| report.target == by).unwrap();
            assert!(
                cause.finding.is_some(),
                "{key} is blocked by a healthy {by}"
            );
        }
    }
}
