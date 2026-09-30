use std::path::Path;
use std::sync::Arc;

use mix_core::action::Failure;
use mix_core::change::{Change, NewerList, Unrenderable};
use mix_core::models::UserConfig;
use mix_core::plan::{Runner, Verdict, diagnostic};
use mix_events::v1::{Command, command};
use mix_events::{Ending, Outbox, ROOT, Start, Tree};

use crate::Context;
use crate::bridge::Bridge;
use crate::drive::{Journal, Observer, Performer, drive, stopped_by};
use crate::effect::files::Files;
use crate::effect::generations::ProfileContext;
use crate::ops::bootstrap::request_id;
use crate::profile::state::{self, Invalid, Settled};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error(transparent)]
    InvalidPackage(#[from] mix_nixgen::InvalidInput),

    #[error("the package list would not be valid: {0}")]
    InvalidState(#[from] Invalid),

    #[error("the package list was written by a newer version of mix (format {0})")]
    NewerState(u32),

    #[error("this command changes the invoking user's profile, and it was run as root")]
    NotRoot,

    #[error("no managed environment was found for the invoking user")]
    NotBootstrapped,
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<Unrenderable> for Error {
    fn from(error: Unrenderable) -> Self {
        match error {
            Unrenderable::Package(error) => Error::InvalidPackage(error),
            Unrenderable::State(invalid) => Error::InvalidState(invalid),
        }
    }
}

impl From<NewerList> for Error {
    fn from(NewerList(version): NewerList) -> Self {
        Error::NewerState(version)
    }
}

pub fn settled(cfg: &UserConfig) -> Settled {
    state::settle(&cfg.user.home)
}

pub enum Verb {
    Install,
    Remove,
}

impl Verb {
    fn label(&self) -> &'static str {
        match self {
            Verb::Install => "Installing",
            Verb::Remove => "Removing",
        }
    }

    fn request(&self, packages: &[String]) -> command::Request {
        let packages = packages.to_vec();
        match self {
            Verb::Install => command::Request::Install(mix_events::v1::InstallRequest { packages }),
            Verb::Remove => command::Request::Remove(mix_events::v1::RemoveRequest { packages }),
        }
    }

    fn key(&self) -> &'static str {
        match self {
            Verb::Install => "install",
            Verb::Remove => "remove",
        }
    }
}

pub async fn run(
    ctx: &Context,
    cfg: &UserConfig,
    verb: Verb,
    requested: &[String],
    change: &Change,
    journal: &mut dyn Journal,
) -> Result<()> {
    let steps = mix_core::change::steps(&cfg.user, change, verb.label())?;
    if steps.is_empty() {
        return Ok(());
    }
    let scope = &ctx.scope;
    let request = request_id();
    let files = Files::open(Path::new("/"), &request).map_err(|source| mix_core::Error::Io {
        path: "/".into(),
        source,
    })?;
    let mut performer = Performer::new(files).with_profile(ProfileContext {
        mirror: ctx.mirror().map(str::to_string),
        host: ctx.host.clone(),
    });
    let outbox = Arc::new(Outbox::new(request, || {}));
    let mut bridge = Bridge::new(Arc::clone(&outbox), ctx.reporters.clone());
    let stopped = stopped_by(scope);
    let mut tree = Tree::new(
        outbox,
        Arc::clone(&stopped),
        Start::command(
            verb.key(),
            Command {
                mix_version: env!("CARGO_PKG_VERSION").to_string(),
                schema_minor: mix_events::SCHEMA_MINOR,
                request: Some(verb.request(requested)),
            },
        ),
    );
    let mut runner = Runner::new(ROOT, steps);
    let report = drive(
        &mut runner,
        &mut tree,
        &mut performer,
        scope,
        &stopped,
        journal,
        &mut bridge,
    )
    .await;
    let (ending, outcome) = match &report.verdict {
        Verdict::Succeeded => (Ending::succeeded(), Ok(())),
        Verdict::Failed { failure, .. } => (
            Ending::failed(diagnostic(failure)),
            Err(error_of(failure.clone())),
        ),
        Verdict::Cancelled(cause) => (
            Ending::cancelled(*cause),
            Err(Error::Core(mix_core::Error::Cancelled {
                command: format!("mix {}", verb.key()),
            })),
        ),
    };
    let _ = tree.finish(ROOT, ending);
    drop(tree);
    bridge.flush();
    outcome
}

fn error_of(failure: Failure) -> Error {
    Error::Core(match failure {
        Failure::Io { path, kind } => mix_core::Error::Io {
            path,
            source: kind.into(),
        },
        Failure::SpawnFailed { program, kind } => mix_core::Error::Exec {
            command: program,
            source: kind.into(),
        },
        Failure::CommandFailed {
            program,
            output_tail,
            ..
        } => mix_core::Error::Command {
            command: program,
            detail: output_tail,
        },
        Failure::Cancelled => mix_core::Error::Cancelled {
            command: "mix".to_string(),
        },
        other => mix_core::Error::Command {
            command: "mix".to_string(),
            detail: diagnostic(&other).message,
        },
    })
}
