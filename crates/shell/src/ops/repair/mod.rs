//! The reconciliation behind `mix repair`: every declared target measured, and the ones that
//! drifted put back.
//!
//! What can be measured and what can be put back are [`crate::target`]'s, not this command's:
//! `mix repair` decides the order it walks the environment in, what a change means for the
//! nix-daemon and for the git-tracked state, and what it hands the caller to print.

use std::path::Path;

use mix_core::action::Failure;
use mix_core::health;
use mix_core::models::{Target, UserConfig, targets};
use mix_core::paths::mix_state_dir;
use mix_core::plan::{Runner, StepOutcome, Verdict};
use mix_events::v1::{Cancellation, Code, RepairResult, node_finished};
use mix_events::{Diagnose, Ending, Fault, ROOT, Stopped, Tree};
use mix_exec::Scope;

use crate::drive::{Journal, Observer, Performer, drive};
use crate::effect::files::Files;
use crate::effect::git;
use crate::effect::home::core_error;
use crate::effect::journal::{FileJournal, JOURNAL_DIR, recover_all};
use crate::render::Relay;
use crate::request::{Concluded, Root};
use crate::target::Error;
use crate::{Context, HostConfig};

pub struct RepairReport {
    pub name: String,
    pub fixed: bool,
    /// What stopped the repair, handed over rather than rendered: the caller decides how it
    /// should read, and a typed error is still there to be matched on.
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

pub(crate) async fn repair(ctx: &Context, root: &mut Root) -> Concluded<Repair> {
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
    root.conclude_with_problems(ending.with_result(result), problems_remain, repair)
}

async fn repaired(
    ctx: &Context,
    request: &str,
    tree: &mut Tree,
    stopped: &Stopped,
    observer: &mut Relay,
) -> Repair {
    let user_config = ctx.user.as_ref();
    let scope = &ctx.scope;
    let items = targets(user_config, &ctx.policy);
    let files = match Files::open(Path::new("/"), request) {
        Ok(files) => files,
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
    let mut performer = Performer::new(files);
    let journals = Path::new(JOURNAL_DIR);
    let recovered = recover_all(journals, &mut performer, &scope.shielded()).await;
    for (_, failure) in &recovered.failures {
        let _ = tree.warn(
            ROOT,
            mix_core::diagnose::warning(
                Code::CleanupIncomplete,
                "could not finish an interrupted request",
                failure,
            ),
        );
    }
    let mut journal = match FileJournal::create(journals, request) {
        Ok(journal) => journal,
        Err(failure) => {
            return Repair {
                reports: vec![RepairReport::failed(
                    JOURNAL_DIR,
                    error_of(failure, JOURNAL_DIR),
                )],
                interrupted: false,
            };
        }
    };
    let (mut reports, interrupted) = put_back(
        items,
        request,
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
    if let Err(failure) = journal.finish() {
        let _ = tree.warn(
            ROOT,
            mix_core::diagnose::warning(
                Code::CleanupIncomplete,
                "could not remove the finished journal",
                &failure,
            ),
        );
    }

    if let Some(cfg) = user_config {
        commit_the_tracked_state(cfg, &ctx.host, &scope.shielded(), &mut reports).await;
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
    items: Vec<Target<'_>>,
    request: &str,
    performer: &mut Performer,
    journal: &mut dyn Journal,
    scope: &Scope,
    events: Events<'_>,
) -> (Vec<RepairReport>, bool) {
    let mut runner = Runner::new(ROOT, health::repair_steps(items, request)).independent();
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
    for (name, failure) in std::mem::take(&mut report.rollback_failures) {
        let error = error_of(failure, &name);
        reports.push(RepairReport::failed(name, error));
    }
    (reports, matches!(report.verdict, Verdict::Cancelled(_)))
}

/// Configuration mix rewrote is drift the user should be able to see in git.
async fn commit_the_tracked_state(
    cfg: &UserConfig,
    host: &HostConfig,
    scope: &Scope,
    reports: &mut Vec<RepairReport>,
) {
    const NAME: &str = "git-tracked state";

    let state_dir = mix_state_dir(&cfg.user.home);
    let git = git::Git::resolve(&cfg.user, host.git_binary.as_deref()).await;
    match git.sync(&cfg.user, &state_dir, scope).await {
        Ok(true) => reports.push(RepairReport::repaired(NAME)),
        Ok(false) => {}
        Err(e) => reports.push(RepairReport::failed(NAME, e)),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::journal::Record;

    use std::sync::Arc;

    use mix_events::v1::Command;
    use mix_events::{Start, Tree};

    use super::*;
    use crate::drive::stopped_by;

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
        let mut performer = Performer::new(Files::open(Path::new("/"), "r1").unwrap());
        let mut journal: Vec<Record> = Vec::new();
        let stopped = stopped_by(scope);
        let mut tree = Tree::new(
            Arc::new(mix_events::Outbox::new("r1", || {})),
            Arc::clone(&stopped),
            Start::command("repair", Command::default()),
        );
        put_back(
            targets,
            "r1",
            &mut performer,
            &mut journal,
            scope,
            Events {
                tree: &mut tree,
                stopped: &stopped,
                observer: &mut (),
            },
        )
        .await
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
