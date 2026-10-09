//! The reconciliation behind `mix repair`: every declared target measured, and the ones that
//! drifted put back.
//!
//! What can be measured and what can be put back are [`crate::target`]'s, not this command's:
//! `mix repair` decides the order it walks the environment in, what a change means for the
//! nix-daemon and for the git-tracked state, and what it reports for the client to show.

use std::path::Path;

use mix_core::action::Failure;
use mix_core::health;

use crate::profile::config::observed_user_config;
use mix_core::plan::{Runner, StepOutcome, Verdict};
use mix_core::targets::targets;
use mix_events::v1::{Cancellation, Code, RepairRequest, RepairResult, node_finished};
use mix_events::{Diagnose, Ending, Fault, ROOT, Stopped, Tree};
use mix_exec::Scope;

use crate::Context;
use crate::drive::{Journal, Observer, Performer, drive};
use crate::effect::generations::ProfileContext;
use crate::effect::home::core_error;

use crate::request::sink::Relay;
use crate::request::{Concluded, Root};
use crate::target::Error;

pub struct RepairReport {
    pub name: String,
    pub fixed: bool,
    /// What stopped the repair. It reaches the client as a diagnostic, and the client decides
    /// how it reads.
    pub error: Option<Error>,
}

impl RepairReport {
    fn wire(&self) -> mix_events::v1::RepairReport {
        mix_events::v1::RepairReport {
            target: self.name.clone(),
            fixed: self.fixed,
            failure: self.error.as_ref().and_then(|error| match error.fault() {
                Fault::Failed(diagnostic) => Some(diagnostic),
                Fault::Cancelled { .. } => None,
            }),
        }
    }

    fn repaired(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fixed: true,
            error: None,
        }
    }

    pub(crate) fn failed(name: impl Into<String>, error: impl Into<Error>) -> Self {
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

pub(crate) async fn repair(ctx: &Context, root: &mut Root, _request: &RepairRequest) -> Concluded {
    let mut observer = ctx.relay();
    let repair = repaired(
        ctx,
        &ctx.request.id,
        &mut root.tree,
        &root.stopped,
        &mut observer,
    )
    .await;
    let result = node_finished::Result::Repair(RepairResult {
        reports: repair.reports.iter().map(RepairReport::wire).collect(),
    });
    let ending = if repair.interrupted {
        Ending::cancelled((root.stopped)().unwrap_or(Cancellation::Interrupted))
    } else {
        Ending::succeeded()
    };
    let problems_remain = !repair.reports.iter().all(|report| report.fixed);
    root.conclude_with_problems(ending.with_result(result), problems_remain)
}

async fn repaired(
    ctx: &Context,
    request: &str,
    tree: &mut Tree,
    stopped: &Stopped,
    observer: &mut Relay,
) -> Repair {
    let scope = &ctx.scope;
    let performer = match ctx.performer() {
        Ok(performer) => performer,
        Err(source) => {
            return Repair {
                reports: vec![RepairReport::failed(
                    "/",
                    Error::Core(mix_core::Error::Io {
                        path: "/".into(),
                        source,
                    }),
                )],
                interrupted: false,
            };
        }
    };
    let mut performer = performer.with_profile(ProfileContext {
        mirror: ctx.mirror().map(str::to_string),
    });
    let journals = ctx.journals.as_path();
    crate::ops::recover_interrupted(ctx, tree, &mut performer, true).await;
    let user_config = match &ctx.user {
        Some(cfg) => observed_user_config(cfg.user.clone(), &mut performer, scope)
            .await
            .or_else(|| ctx.user.clone()),
        None => None,
    };
    let items = targets(user_config.as_ref(), &ctx.policy);
    if ctx.dry_run {
        let (reports, interrupted, _) = put_back(
            health::repair_steps(items, request),
            &mut performer,
            &mut Vec::new(),
            scope,
            Events {
                tree,
                stopped,
                observer,
            },
        )
        .await;
        return Repair {
            reports,
            interrupted,
        };
    }
    let mut journal = match ctx.journal(journals) {
        Ok(journal) => journal,
        Err(failure) => {
            return Repair {
                reports: vec![RepairReport::failed(
                    journals.display().to_string(),
                    error_of(failure, &journals.display().to_string()),
                )],
                interrupted: false,
            };
        }
    };
    let (reports, interrupted, unfinished) = put_back(
        health::repair_steps(items, request),
        &mut performer,
        &mut journal,
        scope,
        Events {
            tree,
            stopped,
            observer,
        },
    )
    .await;
    if !unfinished && let Err(failure) = journal.finish() {
        let _ = tree.warn(
            ROOT,
            mix_core::diagnose::warning(
                Code::CleanupIncomplete,
                "could not remove the finished journal",
                &failure,
            ),
        );
    }

    Repair {
        reports,
        interrupted,
    }
}

fn error_of(failure: Failure, name: &str) -> Error {
    match failure {
        Failure::Unrepairable { artifact, reason } => Error::Unrepairable { artifact, reason },
        other => Error::Core(core_error(other, Path::new(name))),
    }
}

struct Events<'a> {
    tree: &'a mut Tree,
    stopped: &'a Stopped,
    observer: &'a mut dyn Observer,
}

async fn put_back(
    steps: Vec<Box<dyn mix_core::plan::StepSpec>>,
    performer: &mut Performer,
    journal: &mut dyn Journal,
    scope: &Scope,
    events: Events<'_>,
) -> (Vec<RepairReport>, bool, bool) {
    let mut runner = Runner::new(ROOT, steps).independent();
    let report = drive(
        &mut runner,
        events.tree,
        performer,
        scope,
        events.stopped,
        journal,
        events.observer,
    )
    .await;

    let mut reports = Vec::new();
    for (key, outcome) in std::mem::take(&mut report.steps) {
        let restart = key == health::RESTART_NIX_DAEMON;
        let name = if restart {
            std::borrow::Cow::Borrowed("Nix daemon")
        } else {
            key
        };
        match outcome {
            StepOutcome::Changed if restart => {}
            StepOutcome::Changed => reports.push(RepairReport::repaired(name)),
            StepOutcome::Failed(failure) => {
                let error = error_of(failure, &name);
                reports.push(RepairReport::failed(name, error));
            }
            StepOutcome::Satisfied | StepOutcome::Cancelled(_) => {}
        }
    }
    let unfinished = !report.rollback_failures.is_empty();
    for (name, failure) in std::mem::take(&mut report.rollback_failures) {
        let error = error_of(failure, &name);
        reports.push(RepairReport::failed(name, error));
    }
    (
        reports,
        matches!(report.verdict, Verdict::Cancelled(_)),
        unfinished,
    )
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use mix_core::journal::Record;
    use mix_core::targets::Target;

    use std::sync::Arc;

    use mix_events::v1::Command;
    use mix_events::{Start, Tree};

    use super::*;
    use crate::drive::stopped_by;
    use crate::effect::files::Files;

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

    fn drifted_files(dir: &std::path::Path) -> Vec<Target<'static>> {
        ["one", "two"]
            .into_iter()
            .map(|name| {
                let path = dir.join(name);
                std::fs::write(&path, "drifted").unwrap();
                Target::File {
                    path: path.into(),
                    expected: Some("expected".to_string().into()),
                    owner: None,
                }
            })
            .collect()
    }

    async fn repair_in(targets: Vec<Target<'_>>, scope: &Scope) -> (Vec<RepairReport>, bool) {
        let files = Files::open(Path::new("/"), "r1").unwrap();
        let (reports, interrupted, _) = repairing(Performer::new(files), targets, scope).await.0;
        (reports, interrupted)
    }

    async fn repairing(
        mut performer: Performer,
        targets: Vec<Target<'_>>,
        scope: &Scope,
    ) -> ((Vec<RepairReport>, bool, bool), Vec<mix_events::v1::Action>) {
        let mut journal: Vec<Record> = Vec::new();
        let stopped = stopped_by(scope);
        let outbox = Arc::new(mix_events::Outbox::new("r1", || {}));
        let mut tree = Tree::new(
            Arc::clone(&outbox),
            Arc::clone(&stopped),
            Start::command("repair", Command::default()),
        );
        let repaired = put_back(
            health::target_steps(targets, "r1"),
            &mut performer,
            &mut journal,
            scope,
            Events {
                tree: &mut tree,
                stopped: &stopped,
                observer: &mut (),
            },
        )
        .await;
        drop(tree);
        let actions = outbox
            .drain()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Some(mix_events::v1::envelope::Event::NodeStarted(started)) => match started.kind {
                    Some(mix_events::v1::node_started::Kind::Action(action)) => Some(action),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        (repaired, actions)
    }

    #[tokio::test]
    async fn a_dry_run_plans_exactly_what_the_repair_then_does_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut targets = vec![Target::Directory {
            path: blocked.clone().into(),
            mode: 0o755,
            owner: None,
        }];
        targets.extend(drifted_files(dir.path()));
        let before: Vec<(PathBuf, String)> = ["one", "two"]
            .iter()
            .map(|name| {
                let path = dir.path().join(name);
                let contents = std::fs::read_to_string(&path).unwrap();
                (path, contents)
            })
            .collect();
        let scope = mix_exec::Scope::root();

        let (planned, planned_actions) = repairing(
            Performer::predicting(Files::open(Path::new("/"), "r1").unwrap()),
            targets.clone(),
            &scope,
        )
        .await;
        for (path, contents) in &before {
            assert_eq!(&std::fs::read_to_string(path).unwrap(), contents);
        }
        assert_eq!(
            std::fs::metadata(&blocked).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let (done, done_actions) = repairing(
            Performer::new(Files::open(Path::new("/"), "r1").unwrap()),
            targets,
            &scope,
        )
        .await;

        assert!(!planned_actions.is_empty());
        assert_eq!(planned_actions, done_actions);
        let names = |reports: &[RepairReport]| -> Vec<(String, bool)> {
            reports
                .iter()
                .map(|report| (report.name.clone(), report.fixed))
                .collect()
        };
        assert_eq!(names(&planned.0), names(&done.0));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("one")).unwrap(),
            "expected"
        );
    }

    #[tokio::test]
    async fn every_drifted_target_is_put_back_when_nothing_stops_it() {
        let dir = tempfile::tempdir().unwrap();

        let (reports, interrupted) =
            repair_in(drifted_files(dir.path()), &mix_exec::Scope::root()).await;

        assert!(!interrupted);
        let fixed: Vec<&str> = reports
            .iter()
            .filter(|report| report.fixed)
            .map(|report| report.name.as_str())
            .collect();
        assert_eq!(fixed.len(), 2, "{fixed:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("two")).unwrap(),
            "expected"
        );
    }

    #[tokio::test]
    async fn a_stopped_repair_starts_nothing_new_and_says_it_was_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let scope = mix_exec::Scope::root();
        scope.cancel(mix_exec::Reason::Interrupted);

        let (reports, interrupted) = repair_in(drifted_files(dir.path()), &scope).await;

        assert!(interrupted);
        assert!(reports.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("one")).unwrap(),
            "drifted"
        );
    }

    #[tokio::test]
    async fn one_target_that_cannot_be_repaired_does_not_stop_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "a file where a directory belongs").unwrap();
        let mut targets = vec![Target::Directory {
            path: blocked.clone().into(),
            mode: 0o755,
            owner: None,
        }];
        targets.extend(drifted_files(dir.path()));

        let (reports, _) = repair_in(targets, &mix_exec::Scope::root()).await;

        assert!(
            reports
                .iter()
                .any(|report| report.name == blocked.to_string_lossy()
                    && matches!(report.error, Some(Error::Unrepairable { .. })))
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("one")).unwrap(),
            "expected"
        );
    }
}
