mod error;
pub(crate) mod steps;

pub mod detect;
pub mod preflight;
#[doc(hidden)]
pub mod tarball;

pub use error::{Error, Host, Result};

use mix_core::effect::{Digest, Failure};
use mix_core::ops::bootstrap::{Runtime, Settings, steps};
use mix_core::run::{Runner, Verdict, diagnostic};
use mix_events::v1::{BootstrapRequest, BootstrapResult, Code, node_finished};
use mix_events::{Ending, ROOT};

use crate::Context;
use crate::drive::{Performer, drive};
use crate::effect::generations::ProfileContext;
use crate::profile::config::observed_user_config;

use crate::effect::mirror::{filter_mirror, mirror_url};
use crate::request::{Concluded, Root};

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

async fn prepare(ctx: &Context, force: bool) -> Result<(Settings, Performer)> {
    if !ctx.host.is_root() {
        return Err(Error::NotRoot("bootstrap the managed environment"));
    }
    ctx.host.preflight(force, &ctx.scope).await?;
    let request = ctx.request.id.clone();
    let settings = Settings {
        policy: ctx.policy.clone(),
        user: ctx.user.clone(),
        force,
        runtime: runtime(ctx.mirror())?,
        request: request.clone(),
        daemon: std::env::current_exe().map_err(|source| mix_core::Error::Io {
            path: "/proc/self/exe".into(),
            source,
        })?,
    };
    let performer = ctx.performer().map_err(|source| mix_core::Error::Io {
        path: "/".into(),
        source,
    })?;
    let performer = performer.with_profile(ProfileContext {
        mirror: ctx.mirror().map(str::to_string),
    });
    Ok((settings, performer))
}

fn ending_of(verdict: &Verdict) -> Ending {
    match verdict {
        Verdict::Succeeded => {
            Ending::succeeded().with_result(node_finished::Result::Bootstrap(BootstrapResult {
                profile_snippet: mix_core::declared::paths::PROFILE_SNIPPET_DEST.to_string(),
            }))
        }
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
    }
}

pub(crate) async fn bootstrap(
    ctx: &Context,
    root: &mut Root,
    request: &BootstrapRequest,
) -> Concluded {
    let (mut settings, mut performer) = match prepare(ctx, request.force).await {
        Ok(prepared) => prepared,
        Err(error) => return root.refuse(error),
    };
    let scope = &ctx.scope;
    let tree = &mut root.tree;
    let journals = ctx.journals.as_path();
    crate::ops::recover_interrupted(ctx, tree, &mut performer, true).await;
    if let Some(user) = ctx.user.as_ref().map(|cfg| cfg.user.clone()) {
        settings.user = observed_user_config(user, &mut performer, scope)
            .await
            .or(settings.user);
    }
    if ctx.dry_run {
        let mut runner = Runner::new(ROOT, steps(&settings));
        let report = drive(
            &mut runner,
            &mut root.tree,
            &mut performer,
            scope,
            &root.stopped,
            &mut Vec::new(),
            &mut ctx.relay(),
        )
        .await;
        let ending = ending_of(&report.verdict);
        return root.conclude(ending);
    }
    let mut journal = match ctx.journal(journals) {
        Ok(journal) => journal,
        Err(failure) => return root.refuse(error_from(failure)),
    };
    let mut runner = Runner::new(ROOT, steps(&settings));
    let report = drive(
        &mut runner,
        &mut root.tree,
        &mut performer,
        scope,
        &root.stopped,
        &mut journal,
        &mut ctx.relay(),
    )
    .await;
    let ending = ending_of(&report.verdict);
    if report.rollback_failures.is_empty()
        && let Err(failure) = journal.finish()
    {
        let _ = root.tree.warn(
            ROOT,
            mix_core::report::diagnose::warning(
                Code::CleanupIncomplete,
                "could not remove the finished journal",
                &failure,
            ),
        );
    }
    root.conclude(ending)
}
