use std::borrow::Cow;
use std::path::PathBuf;

use mix_nixgen::{CopyIntoGeneration, FileName, HomeModule, InvalidInput, StateVersion};

use crate::declared::identity::InvokingUser;
use crate::declared::paths::{GENERATION_INPUTS, HOME_NIX, STATE_FILE, mix_state_dir};
use crate::declared::state::{REQUIRED_PACKAGES, StateManifest};
use crate::effect::{Action, Fact, Failure, ProfileFacts, Query};
use crate::ops::bootstrap::{FILE_MODE, Facts, ensure_file};
use crate::run::{StepSpec, Title};
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

    fn title(&self) -> Title {
        Title::new(Verb::Writing, "package list")
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
    verb: Verb,
    packages: Vec<String>,
}

impl StepSpec for Activate {
    fn key(&self) -> Cow<'static, str> {
        "activate".into()
    }

    fn title(&self) -> Title {
        Title::new(self.verb, subject(&self.packages))
    }

    fn queries(&self) -> Vec<Query> {
        Vec::new()
    }

    fn actions(&self, _: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok(vec![Action::ActivateProfile {
            user: self.user.clone(),
            source: crate::effect::FlakeSource::Git,
        }])
    }
}

struct Record(InvokingUser);

impl StepSpec for Record {
    fn key(&self) -> Cow<'static, str> {
        "record".into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Recording, "change")
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

pub fn old_generations(profile: &ProfileFacts) -> Vec<u64> {
    profile
        .generations
        .iter()
        .copied()
        .filter(|generation| Some(*generation) != profile.active)
        .collect()
}

struct Prune(InvokingUser);

impl StepSpec for Prune {
    fn key(&self) -> Cow<'static, str> {
        "prune".into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Removing, "old generations")
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Profile(self.0.clone())]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let [Fact::Profile(profile)] = facts else {
            unreachable!("a profile query was answered with {facts:?}");
        };
        Ok(old_generations(profile)
            .into_iter()
            .map(|generation| Action::DeleteGeneration {
                user: self.0.clone(),
                generation,
            })
            .collect())
    }
}

struct Collect(InvokingUser);

impl StepSpec for Collect {
    fn key(&self) -> Cow<'static, str> {
        "collect".into()
    }

    fn title(&self) -> Title {
        Title::new(Verb::Removing, "unused store paths")
    }

    fn queries(&self) -> Vec<Query> {
        Vec::new()
    }

    fn actions(&self, _: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok(vec![Action::CollectGarbage {
            user: self.0.clone(),
        }])
    }
}

pub fn clean_steps(user: &InvokingUser, all: bool) -> Vec<Box<dyn StepSpec>> {
    let mut steps: Vec<Box<dyn StepSpec>> = vec![Box::new(Prune(user.clone()))];
    if all {
        steps.push(Box::new(Collect(user.clone())));
    }
    steps
}

pub fn subject(packages: &[String]) -> String {
    match packages.len() {
        0..=3 => packages.join(", "),
        n => format!("{n} packages"),
    }
}

pub fn steps(
    user: &InvokingUser,
    change: &Change,
    verb: Verb,
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
            verb,
            packages: change.changed.clone(),
        }));
        steps.push(Box::new(Record(user.clone())));
    }
    Ok(steps)
}

#[cfg(test)]
mod tests;
