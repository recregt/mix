use std::borrow::Cow;
use std::path::PathBuf;

use mix_nixgen::{CopyIntoGeneration, FileName, HomeModule, InvalidInput, StateVersion};

use crate::action::{Action, Fact, Failure, Query};
use crate::bootstrap::{FILE_MODE, Facts, ensure_file};
use crate::paths::{GENERATION_STATE_FILE, HOME_NIX, STATE_FILE, mix_state_dir};
use crate::plan::StepSpec;
use crate::privilege::InvokingUser;
use crate::state::{REQUIRED_PACKAGES, StateManifest};

pub const STATE_VERSION: u32 = 1;

pub const HOME_MANAGER_STATE_VERSION: StateVersion = StateVersion::new_static("24.05");

const STATE_INTO_GENERATION: CopyIntoGeneration = CopyIntoGeneration {
    source: FileName::new_static(STATE_FILE),
    target: FileName::new_static(GENERATION_STATE_FILE),
};

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
        },
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
    let manifest = settled.without(&changed);
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
    Ok(
        HomeModule::new(&user.name, &user.home, HOME_MANAGER_STATE_VERSION)?
            .copy_into_generation(STATE_INTO_GENERATION)
            .packages(packages)?
            .render(),
    )
}

pub fn render(user: &InvokingUser, manifest: &StateManifest) -> Result<Rendered, Unrenderable> {
    let home_nix = render_home(user, &manifest.packages)?;
    let state = manifest.render();
    validate(&state)?;
    Ok(Rendered { state, home_nix })
}

struct WriteConfig {
    user: InvokingUser,
    rendered: Rendered,
}

impl WriteConfig {
    fn files(&self) -> [(PathBuf, &str); 2] {
        let state = mix_state_dir(&self.user.home);
        [
            (state.join(STATE_FILE), &self.rendered.state),
            (state.join(HOME_NIX), &self.rendered.home_nix),
        ]
    }
}

impl StepSpec for WriteConfig {
    fn key(&self) -> Cow<'static, str> {
        "write-config".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "write the package list".into()
    }

    fn queries(&self) -> Vec<Query> {
        self.files()
            .into_iter()
            .flat_map(|(path, _)| [Query::Path(path.clone()), Query::Contents(path)])
            .collect()
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let facts = Facts(facts);
        let owner = Some((self.user.uid, self.user.gid));
        let mut actions = Vec::new();
        for (index, (path, wanted)) in self.files().into_iter().enumerate() {
            actions.extend(ensure_file(
                &path,
                facts.path(2 * index),
                facts.contents(2 * index + 1),
                wanted.as_bytes(),
                FILE_MODE,
                owner,
            )?);
        }
        Ok(actions)
    }
}

struct Activate {
    user: InvokingUser,
    title: String,
}

impl StepSpec for Activate {
    fn key(&self) -> Cow<'static, str> {
        "activate".into()
    }

    fn title(&self) -> Cow<'static, str> {
        self.title.clone().into()
    }

    fn queries(&self) -> Vec<Query> {
        Vec::new()
    }

    fn actions(&self, _: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok(vec![Action::ActivateProfile {
            user: self.user.clone(),
        }])
    }
}

struct Record(InvokingUser);

impl StepSpec for Record {
    fn key(&self) -> Cow<'static, str> {
        "record".into()
    }

    fn title(&self) -> Cow<'static, str> {
        "record the change".into()
    }

    fn shielded(&self) -> bool {
        true
    }

    fn queries(&self) -> Vec<Query> {
        Vec::new()
    }

    fn actions(&self, _: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok(vec![Action::RecordState {
            user: self.0.clone(),
        }])
    }
}

pub fn label(verb: &str, packages: &[String]) -> String {
    match packages.len() {
        0..=3 => format!("{verb} {}", packages.join(", ")),
        n => format!("{verb} {n} packages"),
    }
}

pub fn steps(
    user: &InvokingUser,
    change: &Change,
    verb: &str,
) -> Result<Vec<Box<dyn StepSpec>>, Unrenderable> {
    if change.changed.is_empty() && change.source == Source::File {
        return Ok(Vec::new());
    }
    let mut steps: Vec<Box<dyn StepSpec>> = vec![Box::new(WriteConfig {
        user: user.clone(),
        rendered: render(user, &change.manifest)?,
    })];
    if !change.changed.is_empty() {
        steps.push(Box::new(Activate {
            user: user.clone(),
            title: label(verb, &change.changed),
        }));
        steps.push(Box::new(Record(user.clone())));
    }
    Ok(steps)
}

#[cfg(test)]
mod tests;
