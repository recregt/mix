//! The reconciliation behind `mix repair`: every declared target measured, and the ones that
//! drifted put back.
//!
//! What can be measured and what can be put back are [`crate::target`]'s, not this command's:
//! `mix repair` decides the order it walks the environment in, what a change means for the
//! nix-daemon and for the git-tracked state, and what it hands the caller to print.

use std::path::Path;
use std::sync::Arc;

use mix_core::action::Failure;
use mix_core::health;
use mix_core::models::{Target, UserConfig, targets};
use mix_core::paths::mix_state_dir;
use mix_core::plan::{Runner, StepOutcome, Verdict, diagnostic};
use mix_events::v1::{Command, RepairRequest, command};
use mix_events::{Ending, Outbox, ROOT, Start, Tree};
use mix_exec::Scope;

use crate::drive::{Journal, Performer, drive, stopped_by};
use crate::effect::files::Files;
use crate::effect::git;
use crate::effect::home::core_error;
use crate::effect::journal::{FileJournal, JOURNAL_DIR, recover_all};
use crate::ops::bootstrap::request_id;
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

pub async fn repair(ctx: &Context) -> Repair {
    tracing::info!("repairing managed environment");
    let user_config = ctx.user.as_ref();
    let scope = &ctx.scope;
    let request = request_id();
    let items = targets(user_config, &ctx.policy);
    let files = match Files::open(Path::new("/"), &request) {
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
    for (action, failure) in &recovered.failures {
        tracing::warn!("could not finish an interrupted request ({action:?}): {failure:?}");
    }
    let mut journal = match FileJournal::create(journals, &request) {
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
    let (mut reports, interrupted) =
        put_back(items, &request, &mut performer, &mut journal, scope).await;
    if let Err(failure) = journal.finish() {
        tracing::warn!("could not remove the finished journal: {failure:?}");
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

async fn put_back(
    items: Vec<Target<'_>>,
    request: &str,
    performer: &mut Performer,
    journal: &mut dyn Journal,
    scope: &Scope,
) -> (Vec<RepairReport>, bool) {
    let outbox = Arc::new(Outbox::new(request.to_string(), || {}));
    let stopped = stopped_by(scope);
    let mut tree = Tree::new(
        Arc::clone(&outbox),
        Arc::clone(&stopped),
        Start::command(
            "repair",
            Command {
                mix_version: env!("CARGO_PKG_VERSION").to_string(),
                schema_minor: mix_events::SCHEMA_MINOR,
                request: Some(command::Request::Repair(RepairRequest {})),
            },
        ),
    );
    let mut runner = Runner::new(ROOT, health::repair_steps(items, request)).independent();
    let report = drive(
        &mut runner,
        &mut tree,
        performer,
        scope,
        &stopped,
        journal,
        &mut (),
    )
    .await;
    let ending = match &report.verdict {
        Verdict::Succeeded => Ending::succeeded(),
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
    };
    let _ = tree.finish(ROOT, ending);
    drop(tree);
    outbox.drain();

    let mut reports = Vec::new();
    for (name, outcome) in report.steps {
        match outcome {
            StepOutcome::Changed => {
                tracing::debug!("repaired: {name}");
                reports.push(RepairReport::repaired(name));
            }
            StepOutcome::Failed(failure) => {
                tracing::debug!("failed to repair {name}: {failure:?}");
                let error = error_of(failure, &name);
                reports.push(RepairReport::failed(name, error));
            }
            StepOutcome::Satisfied | StepOutcome::Cancelled(_) => {}
        }
    }
    for (name, failure) in report.rollback_failures {
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
    use mix_core::journal::Record;

    use super::*;

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
        put_back(targets, "r1", &mut performer, &mut journal, scope).await
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
