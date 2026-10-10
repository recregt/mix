use std::borrow::Cow;

use mix_nixgen::{CopyIntoGeneration, FileName, HomeModule, InvalidInput, StateVersion};

use crate::declared::identity::InvokingUser;
use crate::declared::paths::GENERATION_INPUTS;
use crate::declared::policy::Policy;
use crate::declared::state::{REQUIRED_PACKAGES, StateManifest};
use crate::declared::targets::{Intent, UserConfig, tree_for};
use crate::effect::ProfileFacts;
use crate::ops::health::reconcile_steps;
use crate::run::StepSpec;
use mix_events::v1::Verb;

pub const STATE_VERSION: u32 = 1;

pub const HOME_MANAGER_STATE_VERSION: StateVersion = StateVersion::new_static("24.05");

const INPUTS_INTO_GENERATION: [CopyIntoGeneration; 4] = {
    let [a, b, c, d] = GENERATION_INPUTS;
    [copy(a), copy(b), copy(c), copy(d)]
};

const fn copy((source, target): (&'static str, &'static str)) -> CopyIntoGeneration {
    CopyIntoGeneration {
        source: FileName::new_static(source),
        target: FileName::new_static(target),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("it is not a package list mix can read: {0}")]
    Unreadable(String),

    #[error("it was written by a newer mix (format {0})")]
    Newer(u32),

    #[error("it has an unknown format ({0})")]
    UnknownVersion(u32),

    #[error("`{0}` is not a package name")]
    Package(String),

    #[error("it does not list `{0}`, which mix needs")]
    Missing(&'static str),
}

pub fn validate(raw: &str) -> Result<StateManifest, Invalid> {
    let manifest =
        StateManifest::parse(raw).map_err(|error| Invalid::Unreadable(error.to_string()))?;
    if manifest.version > STATE_VERSION {
        return Err(Invalid::Newer(manifest.version));
    }
    if manifest.version != STATE_VERSION {
        return Err(Invalid::UnknownVersion(manifest.version));
    }
    if let Some(package) = manifest
        .packages
        .iter()
        .find(|package| !mix_nixgen::is_identifier(package))
    {
        return Err(Invalid::Package(package.clone()));
    }
    if let Some(required) = REQUIRED_PACKAGES
        .iter()
        .find(|required| !manifest.packages.iter().any(|package| package == *required))
    {
        return Err(Invalid::Missing(required));
    }
    Ok(manifest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    File,
    Generation,
    Fresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    Current {
        manifest: StateManifest,
        source: Source,
    },
    Newer(u32),
}

pub fn settle(file: Option<&str>, generation: Option<&str>) -> Settled {
    let file = file.map(validate);
    if let Some(Err(Invalid::Newer(version))) = file {
        return Settled::Newer(version);
    }
    let generation = generation.and_then(|raw| validate(raw).ok());

    let (manifest, source) = match (file, generation) {
        (Some(Ok(file)), Some(generation)) if file != generation => {
            (generation, Source::Generation)
        }
        (Some(Ok(file)), _) => (file, Source::File),
        (_, Some(generation)) => (generation, Source::Generation),
        (_, None) => (StateManifest::seed(), Source::Fresh),
    };
    Settled::Current { manifest, source }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub changed: Vec<String>,
    pub skipped: Vec<String>,
    pub manifest: StateManifest,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the package list was written by a newer version of mix (format {0})")]
pub struct NewerList(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("{} cannot be removed", .0.join(", "))]
    Protected(Vec<String>),

    #[error(transparent)]
    Newer(#[from] NewerList),
}

fn current(settled: Settled) -> Result<(StateManifest, Source), NewerList> {
    match settled {
        Settled::Current { manifest, source } => Ok((manifest, source)),
        Settled::Newer(version) => Err(NewerList(version)),
    }
}

pub fn install(packages: &[String], settled: Settled) -> Result<Change, NewerList> {
    let (settled, source) = current(settled)?;
    let partition = settled.partition(packages);
    let changed: Vec<String> = partition.missing.iter().map(|p| p.to_string()).collect();
    let skipped = partition.installed.iter().map(|p| p.to_string()).collect();
    let mut listed = Vec::with_capacity(settled.packages.len() + changed.len());
    listed.extend_from_slice(&settled.packages);
    listed.extend_from_slice(&changed);
    Ok(Change {
        changed,
        skipped,
        manifest: StateManifest {
            version: settled.version,
            packages: listed,
        }
        .sorted(),
        source,
    })
}

pub fn remove(packages: &[String], settled: Settled) -> Result<Change, Refusal> {
    let protected = StateManifest::protected(packages);
    if !protected.is_empty() {
        return Err(Refusal::Protected(
            protected.into_iter().map(str::to_string).collect(),
        ));
    }
    let (settled, source) = current(settled)?;
    let partition = settled.partition(packages);
    let changed: Vec<String> = partition.installed.iter().map(|p| p.to_string()).collect();
    let skipped = partition.missing.iter().map(|p| p.to_string()).collect();
    let manifest = settled.without(&changed).sorted();
    Ok(Change {
        changed,
        skipped,
        manifest,
        source,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub state: String,
    pub home_nix: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Unrenderable {
    #[error(transparent)]
    Package(#[from] InvalidInput),

    #[error("the package list would not be valid: {0}")]
    State(#[from] Invalid),
}

pub fn render_home<S: AsRef<str>>(
    user: &InvokingUser,
    packages: impl IntoIterator<Item = S>,
) -> Result<String, InvalidInput> {
    let mut module = HomeModule::new(&user.name, &user.home, HOME_MANAGER_STATE_VERSION)?;
    for copy in INPUTS_INTO_GENERATION {
        module = module.copy_into_generation(copy);
    }
    Ok(module.packages(packages)?.render())
}

pub fn render(user: &InvokingUser, manifest: &StateManifest) -> Result<Rendered, Unrenderable> {
    let home_nix = render_home(user, &manifest.packages)?;
    let state = manifest.render();
    validate(&state)?;
    Ok(Rendered { state, home_nix })
}

pub fn old_generations(profile: &ProfileFacts) -> Vec<u64> {
    profile
        .generations
        .iter()
        .copied()
        .filter(|generation| Some(*generation) != profile.active)
        .collect()
}

pub fn subject(packages: &[String]) -> String {
    match packages.len() {
        0..=3 => packages.join(", "),
        n => format!("{n} packages"),
    }
}

pub fn steps(
    cfg: &UserConfig,
    policy: &Policy,
    change: &Change,
    verb: Verb,
    request: &str,
) -> Result<Vec<Box<dyn StepSpec>>, Unrenderable> {
    if change.changed.is_empty() && change.source == Source::File {
        return Ok(Vec::new());
    }
    let rendered = render(&cfg.user, &change.manifest)?;
    let intended = UserConfig {
        home: rendered.home_nix,
        restored_state: Some(rendered.state),
        ..cfg.clone()
    };
    let intent = Intent {
        activating: Some((verb, Cow::Owned(subject(&change.changed)))),
        ..Intent::user(&intended, policy)
    };
    Ok(reconcile_steps(
        Vec::new(),
        tree_for(&intent),
        request,
        Verb::Configuring,
    ))
}

pub fn clean_steps(
    cfg: &UserConfig,
    policy: &Policy,
    all: bool,
    request: &str,
) -> Vec<Box<dyn StepSpec>> {
    let intent = Intent {
        retention: Some(all),
        ..Intent::user(cfg, policy)
    };
    reconcile_steps(Vec::new(), tree_for(&intent), request, Verb::Configuring)
}

#[cfg(test)]
mod tests;
