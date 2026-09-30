mod error;
pub(crate) mod steps;

pub mod detect;
pub mod preflight;
#[doc(hidden)]
pub mod tarball;

pub use error::{Error, Host, Result};

use std::path::Path;
use std::sync::Arc;

use mix_core::action::{Digest, Failure};
use mix_core::bootstrap::{Runtime, Settings, steps};
use mix_core::plan::{Report, Runner, Verdict, diagnostic};
use mix_events::v1::{
    BootstrapRequest, Command, Diagnostic, Severity, Step, command, node_started,
};
use mix_events::{Ending, Outbox, ROOT, Start, Stopped, Tree};

use crate::Context;
use crate::bridge::Bridge;
use crate::drive::{Observer, Performer, drive, stopped_by};
use crate::effect::files::Files;
use crate::effect::generations::ProfileContext;
use crate::effect::journal::{FileJournal, JOURNAL_DIR, recover_all, unfinished};
use crate::effect::mirror::{filter_mirror, mirror_url};

pub struct Environment(());

impl Environment {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

pub(crate) fn request_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn warning(failure: &Failure) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning as i32,
        ..diagnostic(failure)
    }
}

fn digest(hex: &str) -> Result<Digest> {
    let mut bytes = [0u8; 32];
    if hex.len() != 64 {
        return Err(Error::MalformedArchive(format!("pinned digest {hex:?}")));
    }
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * index..2 * index + 2], 16)
            .map_err(|_| Error::MalformedArchive(format!("pinned digest {hex:?}")))?;
    }
    Ok(Digest(bytes))
}

pub fn runtime(mirror: Option<&str>) -> Result<Runtime> {
    let pin = tarball::host_pin()?;
    let file = pin.url.rsplit('/').next().unwrap_or(pin.url);
    Ok(Runtime {
        url: match filter_mirror(mirror) {
            Some(base) => mirror_url(base, file),
            None => pin.url.to_string(),
        },
        sha256: digest(pin.sha256)?,
        size: pin.size,
    })
}

pub fn error_from(failure: Failure) -> Error {
    match failure {
        Failure::Conflict {
            subject,
            expected,
            found,
        } => Error::Conflict {
            subject,
            expected,
            found,
        },
        Failure::Io { path, kind } => Error::Core(mix_core::Error::Io {
            path,
            source: std::io::Error::from(kind),
        }),
        Failure::CommandFailed {
            program,
            output_tail,
            ..
        } => Error::Core(mix_core::Error::Command {
            command: program,
            detail: output_tail,
        }),
        Failure::SpawnFailed { program, kind } => Error::Core(mix_core::Error::Exec {
            command: program,
            source: std::io::Error::from(kind),
        }),
        Failure::Unit(unit) => Error::Unit {
            operation: unit.operation.verb().to_string(),
            detail: format!(
                "job {}, {} ({}), result {}",
                unit.job_result, unit.active_state, unit.sub_state, unit.unit_result
            ),
            unit: unit.unit,
            invocation: unit.invocation,
        },
        Failure::SystemdUnreachable => Error::SystemdUnreachable,
        Failure::Network { url } => Error::Network(Box::new(std::io::Error::other(url))),
        Failure::Integrity {
            artifact,
            expected,
            found,
        } => Error::Integrity {
            artifact,
            detail: format!("expected {expected}, found {found}"),
        },
        Failure::Cancelled => Error::Interrupted,
        Failure::Unrepairable { artifact, reason } => {
            Error::Target(crate::target::Error::Unrepairable { artifact, reason })
        }
    }
}

fn outcome(report: &Report) -> Result<Environment> {
    let cause = match &report.verdict {
        Verdict::Succeeded => return Ok(Environment::new()),
        Verdict::Cancelled(_) => Error::Interrupted,
        Verdict::Failed { failure, .. } => error_from(failure.clone()),
    };
    if report.rollback_failures.is_empty() {
        return Err(cause);
    }
    let summary = report
        .rollback_failures
        .iter()
        .map(|(step, failure)| format!("{step}: {}", diagnostic(failure).message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(Error::Rollback {
        cause: Box::new(cause),
        summary: format!(
            "{} rollback step(s) failed: {summary}",
            report.rollback_failures.len()
        ),
    })
}

pub async fn bootstrap(ctx: &Context, force: bool) -> Result<Environment> {
    let scope = &ctx.scope;
    if !crate::effect::accounts::is_root() {
        return Err(Error::NotRoot("bootstrap the managed environment"));
    }

    preflight::check_not_nixos().await?;
    preflight::check_not_wsl1().await?;
    preflight::check_systemd_ready().await?;
    if !force {
        preflight::check_nix_not_installed(scope).await?;
    }

    let request = request_id();
    let settings = Settings {
        policy: ctx.policy.clone(),
        user: ctx.user.clone(),
        force,
        runtime: runtime(ctx.mirror())?,
        request: request.clone(),
    };
    let files = Files::open(Path::new("/"), &request).map_err(|source| mix_core::Error::Io {
        path: "/".into(),
        source,
    })?;
    let mut performer = Performer::new(files).with_profile(ProfileContext {
        mirror: ctx.mirror().map(str::to_string),
        host: ctx.host.clone(),
    });
    let outbox = Arc::new(Outbox::new(request.clone(), || {}));
    let mut bridge = Bridge::new(Arc::clone(&outbox), ctx.reporters.clone());
    let stopped: Stopped = stopped_by(scope);
    let mut tree = Tree::new(
        outbox,
        Arc::clone(&stopped),
        Start::command(
            "bootstrap",
            Command {
                mix_version: env!("CARGO_PKG_VERSION").to_string(),
                schema_minor: mix_events::SCHEMA_MINOR,
                request: Some(command::Request::Bootstrap(BootstrapRequest {
                    force,
                    mirror: ctx.mirror().map(str::to_string),
                })),
            },
        ),
    );
    let journals = Path::new(JOURNAL_DIR);
    if !unfinished(journals).is_empty() {
        let node = tree
            .start(
                ROOT,
                Start::new(
                    "recover",
                    node_started::Kind::Step(Step {
                        title: "finish an interrupted request".to_string(),
                    }),
                )
                .shielded(),
            )
            .expect("the root is open");
        let recovered = recover_all(journals, &mut performer, &scope.shielded()).await;
        for (_, failure) in &recovered.failures {
            let _ = tree.warn(node, warning(failure));
        }
        let _ = tree.finish(node, Ending::succeeded());
        bridge.flush();
    }
    let mut journal = FileJournal::create(journals, &request).map_err(error_from)?;
    let mut runner = Runner::new(ROOT, steps(&settings));
    let report = drive(
        &mut runner,
        &mut tree,
        &mut performer,
        scope,
        &stopped,
        &mut journal,
        &mut bridge,
    )
    .await;
    let ending = match &report.verdict {
        Verdict::Succeeded => Ending::succeeded(),
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
    };
    if let Err(failure) = journal.finish() {
        let _ = tree.warn(ROOT, warning(&failure));
    }
    let _ = tree.finish(ROOT, ending);
    drop(tree);
    bridge.flush();
    outcome(report)
}
