//! Reading the build plan `nix build --dry-run` prints, so a package that is missing from the
//! binary cache can be refused before nix starts compiling it.
//!
//! nix reports the plan as two lists: the derivations it would build and the paths it would
//! fetch. Only the first matters here, and only partly: home-manager always builds its own
//! generation locally — the profile is assembled on the machine it is for — so those entries are
//! expected and have to be told apart from a package that would really be compiled from source.
//!
//! Most of those entries say so themselves, through the attributes nix uses to decide where a
//! derivation is built. The handful that do not — home-manager's own documentation, planned only
//! on a store that has never built it — are named in [`ALWAYS_LOCAL`].
//!
//! The plan is read in a single pass over the output and nothing is copied except the store
//! paths themselves, and the derivations are classified straight out of `nix derivation show`
//! without deserialising the attributes that the answer does not depend on.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::build_graph::Graph;

const WILL_BUILD_ONE: &str = "this derivation will be built:";
const WILL_BUILD_MANY: &str = "derivations will be built:";
const WILL_FETCH_ONE: &str = "this path will be fetched (";
const WILL_FETCH_MANY: &str = "paths will be fetched (";
const WILL_FETCH_END: &str = "):";
const MANY_OPENER: &str = "these ";
const CANNOT_BUILD: &str = "don't know how to build these paths";

const ENTRY_INDENT: &str = "  ";

const STORE_PREFIX: &str = "/nix/store/";
const DRV_SUFFIX: &str = ".drv";

/// Length of the hash nix puts at the front of a store path name, plus its dash.
const HASH_PREFIX_LEN: usize = 33;

pub const KNOWN_DOCUMENT_VERSIONS: [u64; 1] = [4];

pub const PROFILE_PACKAGES: &str = "home-manager-path";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    Build,
    Fetch,
    CannotBuild,
}

#[derive(Debug)]
struct Open {
    list: List,
    announced: Option<usize>,
    listed: usize,
}

impl Open {
    fn close(self) -> Result<(), PlanError> {
        match self.announced {
            Some(announced) if announced != self.listed => Err(PlanError::Miscounted {
                announced,
                listed: self.listed,
            }),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("{entry} is listed under no header this version of nix is known to print")]
    Unannounced { entry: String },

    #[error("nix does not know how to build {entry}")]
    CannotBuild { entry: String },

    #[error("{entry} is listed as a build but is not a derivation")]
    NotADerivation { entry: String },

    #[error("nix announced {announced} entries in a list and printed {listed}")]
    Miscounted { announced: usize, listed: usize },
}

/// The derivations `nix build --dry-run` says it would build, in the order nix listed them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildPlan {
    to_build: Vec<String>,
}

impl BuildPlan {
    pub fn parse(output: &str) -> Result<Self, PlanError> {
        let mut to_build = Vec::new();
        let mut open: Option<Open> = None;

        for line in output.lines() {
            if let Some(entry) = entry(line) {
                let Some(list) = open.as_mut() else {
                    return Err(PlanError::Unannounced {
                        entry: entry.to_string(),
                    });
                };
                match list.list {
                    List::CannotBuild => {
                        return Err(PlanError::CannotBuild {
                            entry: entry.to_string(),
                        });
                    }
                    List::Build => {
                        let path = entry.trim_end();
                        if !path.ends_with(DRV_SUFFIX) {
                            return Err(PlanError::NotADerivation {
                                entry: path.to_string(),
                            });
                        }
                        to_build.push(path.to_string());
                    }
                    List::Fetch => {}
                }
                list.listed += 1;
                continue;
            }

            if let Some(next) = header(line)
                && let Some(done) = open.replace(next)
            {
                done.close()?;
            }
        }

        if let Some(done) = open {
            done.close()?;
        }
        Ok(Self { to_build })
    }

    pub fn to_build(&self) -> &[String] {
        &self.to_build
    }

    pub fn is_empty(&self) -> bool {
        self.to_build.is_empty()
    }
}

fn entry(line: &str) -> Option<&str> {
    line.strip_prefix(ENTRY_INDENT)
        .filter(|path| path.starts_with(STORE_PREFIX))
}

fn header(line: &str) -> Option<Open> {
    let line = line.trim_end();
    let (list, announced) = if line == WILL_BUILD_ONE {
        (List::Build, Some(1))
    } else if line.starts_with(WILL_FETCH_ONE) && line.ends_with(WILL_FETCH_END) {
        (List::Fetch, Some(1))
    } else if line.starts_with(CANNOT_BUILD) && line.ends_with(':') {
        (List::CannotBuild, None)
    } else {
        let (count, rest) = line.strip_prefix(MANY_OPENER)?.split_once(' ')?;
        let count: usize = count.parse().ok()?;
        if rest == WILL_BUILD_MANY {
            (List::Build, Some(count))
        } else if rest.starts_with(WILL_FETCH_MANY) && rest.ends_with(WILL_FETCH_END) {
            (List::Fetch, Some(count))
        } else {
            return None;
        }
    };

    Some(Open {
        list,
        announced,
        listed: 0,
    })
}

/// Derivations home-manager renders from its own sources, allowed by name.
///
/// These are home-manager's own documentation and message catalogues: they are built with
/// `runCommand`, so they carry neither `preferLocalBuild` nor `allowSubstitutes` and cannot be
/// told apart from a package by their attributes. A store that has them is the normal case and
/// they never reach a plan; a store that does not — a fresh one, or one after a
/// `nix-collect-garbage` — plans them as real builds, and refusing them would make a clean
/// install impossible for the sake of a few seconds of work no cache is going to serve.
///
/// Every name here is unversioned and belongs to home-manager rather than to nixpkgs, so the
/// list cannot be hit by a package a user asked for.
pub const ALWAYS_LOCAL: [&str; 3] = [
    // nixpkgs' `nixosOptionsDoc`, rendered for home-manager's option set.
    "options.json",
    // `man home-configuration.nix`.
    "home-configuration-reference-manpage",
    // The gettext catalogues the activation script's messages are read from.
    "hm-modules-messages",
];

/// Whether a planned derivation is one home-manager always renders here.
///
/// Three length-checked comparisons, and only on the entries the attributes did not already
/// explain, so the allowlist costs nothing on a plan that holds no documentation.
pub fn is_always_local(name: &str) -> bool {
    ALWAYS_LOCAL.contains(&name)
}

/// The readable part of a store path: no directory, no hash, no `.drv`.
pub fn derivation_name(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    let name = file.strip_suffix(DRV_SUFFIX).unwrap_or(file);
    match name.as_bytes().get(HASH_PREFIX_LEN - 1) {
        Some(b'-') => &name[HASH_PREFIX_LEN..],
        _ => name,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unexplained {
    #[error("nix derivation show wrote a document that could not be read: {0}")]
    Unreadable(String),

    #[error(
        "nix derivation show wrote document version {0}, and only {KNOWN_DOCUMENT_VERSIONS:?} are understood"
    )]
    UnknownVersion(u64),

    #[error("nix derivation show said nothing about {0}")]
    Undescribed(String),

    #[error("the planned derivations depend on each other in a cycle")]
    Cycle,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Classified<'a> {
    pub local: Vec<&'a str>,
    pub source: Vec<&'a str>,
    pub packages: Result<Vec<&'a str>, Unexplained>,
}

pub fn classify<'a, P: AsRef<str>>(planned: &'a [P], derivations: &str) -> Classified<'a> {
    let paths: Vec<&'a str> = planned.iter().map(AsRef::as_ref).collect();
    let document = read_document(derivations);

    let is_local: Vec<bool> = paths
        .iter()
        .map(|path| {
            is_always_local(derivation_name(path))
                || document.as_ref().is_ok_and(|derivations| {
                    derivations
                        .get(file_name(path))
                        .is_some_and(Derivation::is_local)
                })
        })
        .collect();

    let mut local = Vec::new();
    let mut source = Vec::new();
    for (path, &built_here) in paths.iter().zip(&is_local) {
        if built_here {
            local.push(*path);
        } else {
            source.push(*path);
        }
    }

    let packages = document.and_then(|derivations| {
        let met = frontier(&paths, &derivations, &is_local)?;
        Ok(met
            .into_iter()
            .map(|index| derivation_name(paths[index]))
            .collect())
    });

    Classified {
        local,
        source,
        packages,
    }
}

fn frontier(
    paths: &[&str],
    derivations: &HashMap<&str, Derivation>,
    is_local: &[bool],
) -> Result<Vec<usize>, Unexplained> {
    let index: HashMap<&str, u32> = paths
        .iter()
        .enumerate()
        .map(|(position, path)| (file_name(path), position as u32))
        .collect();

    let mut edges = Vec::new();
    for (position, path) in paths.iter().enumerate() {
        let derivation = derivations
            .get(file_name(path))
            .ok_or_else(|| Unexplained::Undescribed((*path).to_string()))?;
        edges.extend(
            derivation
                .inputs
                .drvs
                .iter()
                .filter_map(|input| index.get(file_name(input)))
                .map(|&dependency| (position as u32, dependency)),
        );
    }

    let graph = Graph::new(paths.len(), &edges);
    let order = graph.dependents_first().ok_or(Unexplained::Cycle)?;
    let compiled = |node: usize| !is_local[node];

    let packages: Vec<usize> = profile_packages(paths, derivations)
        .into_iter()
        .filter(|&node| compiled(node))
        .collect();
    if !packages.is_empty() {
        return Ok(packages);
    }
    Ok(graph.frontier(&order, compiled))
}

fn profile_packages(paths: &[&str], derivations: &HashMap<&str, Derivation>) -> Vec<usize> {
    let chosen: HashSet<&str> = paths
        .iter()
        .filter(|path| derivation_name(path) == PROFILE_PACKAGES)
        .filter_map(|path| derivations.get(file_name(path)))
        .flat_map(|profile| profile.chosen_outputs().iter().map(|path| file_name(path)))
        .collect();
    if chosen.is_empty() {
        return Vec::new();
    }

    paths
        .iter()
        .enumerate()
        .filter(|(_, path)| {
            derivations.get(file_name(path)).is_some_and(|derivation| {
                derivation
                    .outputs
                    .iter()
                    .any(|output| chosen.contains(file_name(output)))
            })
        })
        .map(|(position, _)| position)
        .collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn read_document(raw: &str) -> Result<HashMap<&str, Derivation<'_>>, Unexplained> {
    match serde_json::from_str::<Document>(raw) {
        Ok(document) if KNOWN_DOCUMENT_VERSIONS.contains(&document.version) => {
            Ok(document.derivations)
        }
        Ok(document) => Err(Unexplained::UnknownVersion(document.version)),
        Err(error) => match serde_json::from_str::<Versioned>(raw) {
            Ok(versioned) if !KNOWN_DOCUMENT_VERSIONS.contains(&versioned.version) => {
                Err(Unexplained::UnknownVersion(versioned.version))
            }
            _ => Err(Unexplained::Unreadable(error.to_string())),
        },
    }
}

#[derive(Deserialize)]
struct Versioned {
    version: u64,
}

#[derive(Deserialize)]
struct Document<'a> {
    version: u64,
    #[serde(borrow)]
    derivations: HashMap<&'a str, Derivation<'a>>,
}

#[derive(Deserialize)]
struct Derivation<'a> {
    #[serde(default)]
    env: BuildFlags,
    // Derivations built with `__structuredAttrs` carry their attributes here instead.
    #[serde(default, rename = "structuredAttrs")]
    structured_attrs: Option<BuildFlags>,
    #[serde(borrow)]
    inputs: Inputs<'a>,
    #[serde(default, borrow)]
    outputs: Outputs<'a>,
}

impl Derivation<'_> {
    fn chosen_outputs(&self) -> &[String] {
        match &self.structured_attrs {
            Some(attrs) if !attrs.chosen_outputs.is_empty() => &attrs.chosen_outputs,
            _ => &self.env.chosen_outputs,
        }
    }

    fn is_local(&self) -> bool {
        let structured = self.structured_attrs.as_ref();
        let prefers_local = structured
            .and_then(|attrs| attrs.prefer_local_build)
            .or(self.env.prefer_local_build);
        let allows_substitutes = structured
            .and_then(|attrs| attrs.allow_substitutes)
            .or(self.env.allow_substitutes);

        prefers_local == Some(true) || allows_substitutes == Some(false)
    }
}

#[derive(Deserialize)]
struct Inputs<'a> {
    #[serde(borrow)]
    drvs: Keys<'a>,
}

struct Keys<'a>(Vec<Cow<'a, str>>);

impl<'a> Keys<'a> {
    fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(AsRef::as_ref)
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Keys<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeysVisitor<'a>(std::marker::PhantomData<&'a ()>);

        impl<'de: 'a, 'a> Visitor<'de> for KeysVisitor<'a> {
            type Value = Keys<'a>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map keyed by derivation")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys<'a>, A::Error> {
                let mut keys = Vec::new();
                while let Some(Key(key)) = map.next_key::<Key<'de>>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(Keys(keys))
            }
        }

        deserializer.deserialize_map(KeysVisitor(std::marker::PhantomData))
    }
}

#[derive(Default)]
struct Outputs<'a>(Vec<Cow<'a, str>>);

impl<'a> Outputs<'a> {
    fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(AsRef::as_ref)
    }
}

#[derive(Deserialize)]
struct Output<'a> {
    #[serde(default, borrow)]
    path: Option<Key<'a>>,
}

impl<'de: 'a, 'a> Deserialize<'de> for Outputs<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OutputsVisitor<'a>(std::marker::PhantomData<&'a ()>);

        impl<'de: 'a, 'a> Visitor<'de> for OutputsVisitor<'a> {
            type Value = Outputs<'a>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map of derivation outputs")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Outputs<'a>, A::Error> {
                let mut paths = Vec::new();
                while map.next_key::<IgnoredAny>()?.is_some() {
                    if let Some(Key(path)) = map.next_value::<Output<'de>>()?.path {
                        paths.push(path);
                    }
                }
                Ok(Outputs(paths))
            }
        }

        deserializer.deserialize_map(OutputsVisitor(std::marker::PhantomData))
    }
}

struct Key<'a>(Cow<'a, str>);

impl<'de: 'a, 'a> Deserialize<'de> for Key<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor<'a>(std::marker::PhantomData<&'a ()>);

        impl<'de: 'a, 'a> Visitor<'de> for KeyVisitor<'a> {
            type Value = Key<'a>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a derivation path")
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Key<'a>, E> {
                Ok(Key(Cow::Borrowed(value)))
            }

            fn visit_str<E>(self, value: &str) -> Result<Key<'a>, E> {
                Ok(Key(Cow::Owned(value.to_string())))
            }
        }

        deserializer.deserialize_str(KeyVisitor(std::marker::PhantomData))
    }
}

/// The two attributes, wherever nix chose to write them.
#[derive(Default)]
struct BuildFlags {
    prefer_local_build: Option<bool>,
    allow_substitutes: Option<bool>,
    chosen_outputs: Vec<String>,
}

impl<'de> Deserialize<'de> for BuildFlags {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FlagsVisitor;

        impl<'de> Visitor<'de> for FlagsVisitor {
            type Value = BuildFlags;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a derivation attribute set")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<BuildFlags, A::Error> {
                let mut flags = BuildFlags::default();
                while let Some(key) = map.next_key::<&str>()? {
                    match key {
                        "preferLocalBuild" => {
                            flags.prefer_local_build = Some(map.next_value::<NixFlag>()?.0);
                        }
                        "allowSubstitutes" => {
                            flags.allow_substitutes = Some(map.next_value::<NixFlag>()?.0);
                        }
                        "chosenOutputs" => {
                            flags.chosen_outputs =
                                chosen_outputs(&map.next_value::<serde_json::Value>()?);
                        }
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(flags)
            }
        }

        deserializer.deserialize_map(FlagsVisitor)
    }
}

fn chosen_outputs(value: &serde_json::Value) -> Vec<String> {
    let Some(groups) = value.as_array() else {
        return Vec::new();
    };
    groups
        .iter()
        .filter_map(|group| group.get("paths")?.as_array())
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect()
}

/// A derivation attribute read as a flag.
///
/// Structured attributes carry a real boolean; a plain derivation carries the string nix encodes
/// one as, `"1"` for true and `""` for false.
struct NixFlag(bool);

impl<'de> Deserialize<'de> for NixFlag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FlagVisitor;

        impl<'de> Visitor<'de> for FlagVisitor {
            type Value = NixFlag;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a derivation attribute holding a flag")
            }

            fn visit_bool<E>(self, value: bool) -> Result<NixFlag, E> {
                Ok(NixFlag(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<NixFlag, E> {
                Ok(NixFlag(!value.is_empty() && value != "0"))
            }

            fn visit_u64<E>(self, value: u64) -> Result<NixFlag, E> {
                Ok(NixFlag(value != 0))
            }

            fn visit_i64<E>(self, value: i64) -> Result<NixFlag, E> {
                Ok(NixFlag(value != 0))
            }

            fn visit_unit<E>(self) -> Result<NixFlag, E> {
                Ok(NixFlag(false))
            }

            fn visit_none<E>(self) -> Result<NixFlag, E> {
                Ok(NixFlag(false))
            }
        }

        deserializer.deserialize_any(FlagVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRY_RUN_MANY: &str = include_str!("../fixtures/nix/dry-run-many.txt");
    const DRY_RUN_ONE: &str = include_str!("../fixtures/nix/dry-run-one.txt");
    const DERIVATION_SHOW: &str = include_str!("../fixtures/nix/derivation-show.json");

    const PLAN: &str = "\
these 2 derivations will be built:
  /nix/store/00000000000000000000000000000001-home-manager-path.drv
  /nix/store/00000000000000000000000000000002-hello-2.12.3.drv
these 2 paths will be fetched (1.5 MiB download, 4.0 MiB unpacked):
  /nix/store/00000000000000000000000000000003-fd-10.5.0
  /nix/store/00000000000000000000000000000004-git-2.51.0
";

    fn names<'a>(paths: &[&'a str]) -> Vec<&'a str> {
        paths.iter().map(|path| derivation_name(path)).collect()
    }

    #[test]
    fn the_plan_nix_printed_is_read_in_full() {
        let plan = BuildPlan::parse(DRY_RUN_MANY).unwrap();

        assert_eq!(plan.to_build().len(), 9);
        assert!(
            plan.to_build()
                .iter()
                .all(|path| path.ends_with(DRV_SUFFIX))
        );
    }

    #[test]
    fn a_single_planned_derivation_nix_printed_is_read() {
        let plan = BuildPlan::parse(DRY_RUN_ONE).unwrap();

        assert_eq!(
            names(
                &plan
                    .to_build()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            ),
            ["leaf-1.0"]
        );
    }

    #[test]
    fn a_plan_lists_only_the_derivations_that_would_be_built() {
        let plan = BuildPlan::parse(PLAN).unwrap();
        assert_eq!(
            plan.to_build(),
            [
                "/nix/store/00000000000000000000000000000001-home-manager-path.drv".to_string(),
                "/nix/store/00000000000000000000000000000002-hello-2.12.3.drv".to_string(),
            ]
        );
    }

    #[test]
    fn a_plan_with_nothing_to_do_is_empty() {
        assert!(BuildPlan::parse("").unwrap().is_empty());
        assert!(
            BuildPlan::parse(
                "this path will be fetched (1.5 MiB download, 4.0 MiB unpacked):\n  /nix/store/x\n"
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn a_warning_inside_a_list_does_not_end_it() {
        let plan = BuildPlan::parse(
            "warning: Git tree is dirty\nthese 2 derivations will be built:\n  \
             /nix/store/00000000000000000000000000000001-a.drv\nwarning: still dirty\n  \
             /nix/store/00000000000000000000000000000002-b.drv\n",
        )
        .unwrap();
        assert_eq!(plan.to_build().len(), 2);
    }

    #[test]
    fn a_path_under_a_header_nix_is_not_known_to_print_is_refused() {
        let error = BuildPlan::parse(
            "these 1 derivations are going to be built:\n  \
             /nix/store/00000000000000000000000000000001-a.drv\n",
        )
        .unwrap_err();

        assert!(matches!(error, PlanError::Unannounced { .. }));
    }

    #[test]
    fn a_path_after_a_renamed_header_is_not_counted_into_the_list_before_it() {
        let error = BuildPlan::parse(
            "this path will be fetched (1.5 MiB download, 4.0 MiB unpacked):\n  /nix/store/x\n\
             these 2 derivations are going to be built:\n  \
             /nix/store/00000000000000000000000000000001-a.drv\n  \
             /nix/store/00000000000000000000000000000002-b.drv\n",
        )
        .unwrap_err();

        assert_eq!(
            error,
            PlanError::Miscounted {
                announced: 1,
                listed: 3
            }
        );
    }

    #[test]
    fn a_list_shorter_than_announced_is_refused() {
        let error = BuildPlan::parse(
            "these 3 derivations will be built:\n  \
             /nix/store/00000000000000000000000000000001-a.drv\n",
        )
        .unwrap_err();

        assert_eq!(
            error,
            PlanError::Miscounted {
                announced: 3,
                listed: 1
            }
        );
    }

    #[test]
    fn a_path_nix_cannot_build_is_refused() {
        let error = BuildPlan::parse(
            "don't know how to build these paths (may be caused by read-only store access):\n  \
             /nix/store/00000000000000000000000000000001-a\n",
        )
        .unwrap_err();

        assert!(matches!(error, PlanError::CannotBuild { .. }));
    }

    #[test]
    fn a_build_that_is_not_a_derivation_is_refused() {
        let error = BuildPlan::parse(
            "this derivation will be built:\n  /nix/store/00000000000000000000000000000001-a\n",
        )
        .unwrap_err();

        assert!(matches!(error, PlanError::NotADerivation { .. }));
    }

    #[test]
    fn a_line_that_is_not_a_store_path_is_not_an_entry() {
        let plan = BuildPlan::parse(
            "this derivation will be built:\n  /nix/store/00000000000000000000000000000001-a.drv\n  \
             not-a-store-path.drv\n",
        )
        .unwrap();
        assert_eq!(plan.to_build().len(), 1);
    }

    #[test]
    fn a_derivation_name_drops_the_directory_the_hash_and_the_suffix() {
        assert_eq!(
            derivation_name("/nix/store/00000000000000000000000000000001-hello-2.12.3.drv"),
            "hello-2.12.3"
        );
    }

    #[test]
    fn a_derivation_name_survives_a_path_that_carries_no_hash() {
        assert_eq!(derivation_name("hello.drv"), "hello");
        assert_eq!(derivation_name(""), "");
    }

    fn fixture_plan() -> Vec<String> {
        BuildPlan::parse(DRY_RUN_MANY).unwrap().to_build().to_vec()
    }

    #[test]
    fn the_fixture_is_split_into_what_is_built_here_and_what_is_compiled() {
        let planned = fixture_plan();
        let classified = classify(&planned, DERIVATION_SHOW);

        let mut local = names(&classified.local);
        local.sort_unstable();
        let mut source = names(&classified.source);
        source.sort_unstable();

        assert_eq!(
            local,
            [
                "home-manager-generation",
                "home-manager-path",
                "options.json",
                "tool-wrapper"
            ]
        );
        assert_eq!(
            source,
            [
                "leaf-1.0",
                "library-2.1",
                "package-3.2",
                "shell-5.3",
                "tool-4.0"
            ]
        );
    }

    #[test]
    fn the_packages_named_are_the_compiled_packages_of_the_profile() {
        let planned = fixture_plan();
        let mut packages = classify(&planned, DERIVATION_SHOW).packages.unwrap();
        packages.sort_unstable();

        assert_eq!(packages, ["library-2.1", "package-3.2"]);
    }

    fn profile(chosen: &[&str], inputs: &[&str]) -> String {
        let chosen: Vec<String> = chosen
            .iter()
            .map(|output| format!("{{\"paths\":[\"/nix/store/{output}\"],\"priority\":5}}"))
            .collect();
        let drvs: Vec<String> = inputs
            .iter()
            .map(|input| format!("\"{input}\":{{\"dynamicOutputs\":{{}},\"outputs\":[\"out\"]}}"))
            .collect();
        format!(
            "{{\"env\":{{}},\"structuredAttrs\":{{\"preferLocalBuild\":true,\"chosenOutputs\":[{}]}},\
             \"inputs\":{{\"drvs\":{{{}}},\"srcs\":[]}}}}",
            chosen.join(","),
            drvs.join(",")
        )
    }

    fn package(output: &str, inputs: &[&str]) -> String {
        let drvs: Vec<String> = inputs
            .iter()
            .map(|input| format!("\"{input}\":{{\"dynamicOutputs\":{{}},\"outputs\":[\"out\"]}}"))
            .collect();
        format!(
            "{{\"env\":{{}},\"inputs\":{{\"drvs\":{{{}}},\"srcs\":[]}},\
             \"outputs\":{{\"out\":{{\"path\":\"{output}\"}}}}}}",
            drvs.join(",")
        )
    }

    fn output(index: usize, name: &str) -> String {
        format!("{index:032}-{name}")
    }

    #[test]
    fn a_compile_home_manager_needs_for_itself_is_not_named_beside_a_package() {
        let planned = vec![
            path(1, "home-manager-generation"),
            path(2, "home-manager-path"),
            path(3, "cowsay-3.8.4"),
            path(4, "bash-5.3p15"),
        ];
        let derivations = shown(&[
            (
                &key(1, "home-manager-generation"),
                &derivation(
                    r#"{"preferLocalBuild":"1"}"#,
                    &[&key(2, "home-manager-path"), &key(4, "bash-5.3p15")],
                ),
            ),
            (
                &key(2, "home-manager-path"),
                &profile(
                    &[&output(3, "cowsay-3.8.4")],
                    &[&key(3, "cowsay-3.8.4"), &key(4, "bash-5.3p15")],
                ),
            ),
            (
                &key(3, "cowsay-3.8.4"),
                &package(&output(3, "cowsay-3.8.4"), &[]),
            ),
            (
                &key(4, "bash-5.3p15"),
                &package(&output(4, "bash-5.3p15"), &[]),
            ),
        ]);

        let classified = classify(&planned, &derivations);
        assert_eq!(classified.source.len(), 2);
        assert_eq!(classified.packages, Ok(vec!["cowsay-3.8.4"]));
    }

    #[test]
    fn a_local_package_home_manager_adds_does_not_bring_its_builder_into_the_message() {
        let planned = vec![
            path(1, "home-manager-path"),
            path(2, "dummy-xdg-mime-dirs1"),
            path(3, "bash-5.3p15"),
            path(4, "cowsay-3.8.4"),
        ];
        let derivations = shown(&[
            (
                &key(1, "home-manager-path"),
                &profile(
                    &[
                        &output(2, "dummy-xdg-mime-dirs1"),
                        &output(4, "cowsay-3.8.4"),
                    ],
                    &[&key(2, "dummy-xdg-mime-dirs1"), &key(4, "cowsay-3.8.4")],
                ),
            ),
            (
                &key(2, "dummy-xdg-mime-dirs1"),
                &format!(
                    "{{\"env\":{{\"preferLocalBuild\":\"1\"}},\
                     \"inputs\":{{\"drvs\":{{\"{}\":{{}}}},\"srcs\":[]}},\
                     \"outputs\":{{\"out\":{{\"path\":\"{}\"}}}}}}",
                    key(3, "bash-5.3p15"),
                    output(2, "dummy-xdg-mime-dirs1")
                ),
            ),
            (
                &key(3, "bash-5.3p15"),
                &package(&output(3, "bash-5.3p15"), &[]),
            ),
            (
                &key(4, "cowsay-3.8.4"),
                &package(&output(4, "cowsay-3.8.4"), &[]),
            ),
        ]);

        assert_eq!(
            classify(&planned, &derivations).packages,
            Ok(vec!["cowsay-3.8.4"])
        );
    }

    #[test]
    fn a_package_behind_a_local_wrapper_is_named_through_it() {
        let planned = vec![
            path(1, "home-manager-path"),
            path(2, "tool-wrapper"),
            path(3, "tool-4.0"),
        ];
        let derivations = shown(&[
            (
                &key(1, "home-manager-path"),
                &profile(&[&output(2, "tool-wrapper")], &[&key(2, "tool-wrapper")]),
            ),
            (
                &key(2, "tool-wrapper"),
                &format!(
                    "{{\"env\":{{\"preferLocalBuild\":\"1\"}},\
                     \"inputs\":{{\"drvs\":{{\"{}\":{{}}}},\"srcs\":[]}},\
                     \"outputs\":{{\"out\":{{\"path\":\"{}\"}}}}}}",
                    key(3, "tool-4.0"),
                    output(2, "tool-wrapper")
                ),
            ),
            (&key(3, "tool-4.0"), &package(&output(3, "tool-4.0"), &[])),
        ]);

        assert_eq!(
            classify(&planned, &derivations).packages,
            Ok(vec!["tool-4.0"])
        );
    }

    #[test]
    fn a_package_list_of_an_unexpected_shape_falls_back_to_the_whole_plan() {
        let planned = vec![path(1, "home-manager-path"), path(2, "bash-5.3p15")];
        let derivations = shown(&[
            (
                &key(1, "home-manager-path"),
                &format!(
                    "{{\"env\":{{}},\"structuredAttrs\":{{\"preferLocalBuild\":true,\
                     \"chosenOutputs\":\"not a list\"}},\
                     \"inputs\":{{\"drvs\":{{\"{}\":{{}}}},\"srcs\":[]}}}}",
                    key(2, "bash-5.3p15")
                ),
            ),
            (
                &key(2, "bash-5.3p15"),
                &package(&output(2, "bash-5.3p15"), &[]),
            ),
        ]);

        let classified = classify(&planned, &derivations);
        assert_eq!(classified.local.len(), 1);
        assert_eq!(classified.packages, Ok(vec!["bash-5.3p15"]));
    }

    #[test]
    fn without_a_package_to_blame_the_first_compiles_of_the_whole_plan_are_named() {
        let planned = vec![
            path(1, "home-manager-generation"),
            path(2, "home-manager-path"),
            path(3, "bash-5.3p15"),
        ];
        let derivations = shown(&[
            (
                &key(1, "home-manager-generation"),
                &derivation(
                    r#"{"preferLocalBuild":"1"}"#,
                    &[&key(2, "home-manager-path"), &key(3, "bash-5.3p15")],
                ),
            ),
            (
                &key(2, "home-manager-path"),
                &derivation(r#"{"preferLocalBuild":"1"}"#, &[]),
            ),
            (&key(3, "bash-5.3p15"), &derivation("{}", &[])),
        ]);

        assert_eq!(
            classify(&planned, &derivations).packages,
            Ok(vec!["bash-5.3p15"])
        );
    }

    #[test]
    fn without_the_profile_in_the_plan_the_first_compiles_of_the_whole_plan_are_named() {
        let planned = vec![path(1, "home-manager-generation"), path(2, "bash-5.3p15")];
        let derivations = shown(&[
            (
                &key(1, "home-manager-generation"),
                &derivation(r#"{"preferLocalBuild":"1"}"#, &[&key(2, "bash-5.3p15")]),
            ),
            (&key(2, "bash-5.3p15"), &derivation("{}", &[])),
        ]);

        assert_eq!(
            classify(&planned, &derivations).packages,
            Ok(vec!["bash-5.3p15"])
        );
    }

    #[test]
    fn the_packages_named_follow_the_order_nix_planned_them_in() {
        let planned = fixture_plan();
        let packages = classify(&planned, DERIVATION_SHOW).packages.unwrap();

        let position = |name: &str| {
            planned
                .iter()
                .position(|path| derivation_name(path) == name)
                .unwrap()
        };
        assert!(
            packages
                .windows(2)
                .all(|pair| position(pair[0]) < position(pair[1]))
        );
    }

    fn shown(entries: &[(&str, &str)]) -> String {
        let body: Vec<String> = entries
            .iter()
            .map(|(name, attrs)| format!("\"{name}\":{attrs}"))
            .collect();
        format!("{{\"derivations\":{{{}}},\"version\":4}}", body.join(","))
    }

    fn derivation(env: &str, inputs: &[&str]) -> String {
        let drvs: Vec<String> = inputs
            .iter()
            .map(|input| format!("\"{input}\":{{\"dynamicOutputs\":{{}},\"outputs\":[\"out\"]}}"))
            .collect();
        format!(
            "{{\"env\":{env},\"inputs\":{{\"drvs\":{{{}}},\"srcs\":[]}}}}",
            drvs.join(",")
        )
    }

    fn path(index: usize, name: &str) -> String {
        format!("/nix/store/{index:032}-{name}.drv")
    }

    fn key(index: usize, name: &str) -> String {
        format!("{index:032}-{name}.drv")
    }

    #[test]
    fn a_derivation_that_prefers_a_local_build_is_not_a_source_build() {
        let planned = vec![path(1, "home-manager-generation")];
        let derivations = shown(&[(
            &key(1, "home-manager-generation"),
            &derivation(r#"{"preferLocalBuild":"1"}"#, &[]),
        )]);

        let classified = classify(&planned, &derivations);
        assert!(classified.source.is_empty());
        assert_eq!(classified.packages, Ok(Vec::new()));
    }

    #[test]
    fn a_derivation_that_refuses_substitutes_is_not_a_source_build() {
        let planned = vec![path(1, "home-manager-files")];
        let derivations = shown(&[(
            &key(1, "home-manager-files"),
            &derivation(r#"{"allowSubstitutes":""}"#, &[]),
        )]);

        assert!(classify(&planned, &derivations).source.is_empty());
    }

    #[test]
    fn structured_attributes_are_read_as_booleans() {
        let planned = vec![path(1, "home-manager-path")];
        let derivations = shown(&[(
            &key(1, "home-manager-path"),
            r#"{"env":{"out":"/nix/store/x"},"structuredAttrs":{"preferLocalBuild":true,"allowSubstitutes":false,"buildInputs":["a","b"]},"inputs":{"drvs":{},"srcs":[]}}"#,
        )]);

        assert!(classify(&planned, &derivations).source.is_empty());
    }

    #[test]
    fn a_package_that_would_be_compiled_is_reported_by_name() {
        let planned = vec![path(1, "home-manager-generation"), path(2, "hello-2.12.3")];
        let derivations = shown(&[
            (
                &key(1, "home-manager-generation"),
                &derivation(r#"{"preferLocalBuild":"1"}"#, &[&key(2, "hello-2.12.3")]),
            ),
            (
                &key(2, "hello-2.12.3"),
                &derivation(r#"{"name":"hello"}"#, &[]),
            ),
        ]);

        let classified = classify(&planned, &derivations);
        assert_eq!(names(&classified.source), ["hello-2.12.3"]);
        assert_eq!(classified.packages, Ok(vec!["hello-2.12.3"]));
    }

    #[test]
    fn a_compile_behind_a_local_build_behind_a_compile_is_named_only_once_reached_directly() {
        let planned = vec![
            path(1, "home-manager-path"),
            path(2, "package-1.0"),
            path(3, "local-helper"),
            path(4, "dependency-2.0"),
        ];
        let derivations = shown(&[
            (
                &key(1, "home-manager-path"),
                &derivation(r#"{"preferLocalBuild":"1"}"#, &[&key(2, "package-1.0")]),
            ),
            (
                &key(2, "package-1.0"),
                &derivation("{}", &[&key(3, "local-helper")]),
            ),
            (
                &key(3, "local-helper"),
                &derivation(r#"{"preferLocalBuild":"1"}"#, &[&key(4, "dependency-2.0")]),
            ),
            (&key(4, "dependency-2.0"), &derivation("{}", &[])),
        ]);

        assert_eq!(
            classify(&planned, &derivations).packages,
            Ok(vec!["package-1.0"])
        );
    }

    #[test]
    fn a_plan_longer_than_any_limit_is_classified_in_full() {
        let count = 200;
        let planned: Vec<String> = (0..count).map(|index| path(index, "generated")).collect();
        let entries: Vec<(String, String)> = (0..count)
            .map(|index| {
                let inputs: Vec<String> = (index + 1 < count)
                    .then(|| key(index + 1, "generated"))
                    .into_iter()
                    .collect();
                let inputs: Vec<&str> = inputs.iter().map(String::as_str).collect();
                (
                    key(index, "generated"),
                    derivation(r#"{"preferLocalBuild":"1"}"#, &inputs),
                )
            })
            .collect();
        let entries: Vec<(&str, &str)> = entries
            .iter()
            .map(|(name, attrs)| (name.as_str(), attrs.as_str()))
            .collect();

        let classified = classify(&planned, &shown(&entries));
        assert!(classified.source.is_empty());
        assert_eq!(classified.local.len(), count);
    }

    #[test]
    fn a_derivation_nix_said_nothing_about_is_refused_and_leaves_nothing_to_name() {
        let planned = vec![path(1, "hello-2.12.3")];

        let classified = classify(&planned, &shown(&[]));
        assert_eq!(names(&classified.source), ["hello-2.12.3"]);
        assert!(matches!(
            classified.packages,
            Err(Unexplained::Undescribed(_))
        ));
    }

    #[test]
    fn an_unreadable_document_leaves_every_planned_derivation_refused() {
        let planned = vec![path(1, "hello-2.12.3")];

        let classified = classify(&planned, "not json at all");
        assert_eq!(names(&classified.source), ["hello-2.12.3"]);
        assert!(matches!(
            classified.packages,
            Err(Unexplained::Unreadable(_))
        ));
    }

    #[test]
    fn a_document_of_an_unknown_version_is_not_trusted() {
        let planned = vec![path(1, "home-manager-path")];
        let derivations = format!(
            "{{\"derivations\":{{\"{}\":{}}},\"version\":5}}",
            key(1, "home-manager-path"),
            derivation(r#"{"preferLocalBuild":"1"}"#, &[])
        );

        let classified = classify(&planned, &derivations);
        assert_eq!(names(&classified.source), ["home-manager-path"]);
        assert_eq!(classified.packages, Err(Unexplained::UnknownVersion(5)));
    }

    #[test]
    fn an_unknown_version_is_named_even_when_its_shape_changed_too() {
        let planned = vec![path(1, "hello-2.12.3")];

        let classified = classify(&planned, r#"{"version":9,"drvs":[]}"#);
        assert_eq!(classified.packages, Err(Unexplained::UnknownVersion(9)));
    }

    #[test]
    fn a_bare_map_of_derivations_is_no_longer_trusted() {
        let planned = vec![path(1, "home-manager-path")];
        let derivations = format!(
            "{{\"{}\":{}}}",
            path(1, "home-manager-path"),
            derivation(r#"{"preferLocalBuild":"1"}"#, &[])
        );

        let classified = classify(&planned, &derivations);
        assert_eq!(names(&classified.source), ["home-manager-path"]);
        assert!(classified.packages.is_err());
    }

    #[test]
    fn a_cycle_is_refused_and_leaves_nothing_to_name() {
        let planned = vec![path(1, "a-1.0"), path(2, "b-1.0")];
        let derivations = shown(&[
            (&key(1, "a-1.0"), &derivation("{}", &[&key(2, "b-1.0")])),
            (&key(2, "b-1.0"), &derivation("{}", &[&key(1, "a-1.0")])),
        ]);

        let classified = classify(&planned, &derivations);
        assert_eq!(classified.source.len(), 2);
        assert_eq!(classified.packages, Err(Unexplained::Cycle));
    }

    #[test]
    fn home_managers_own_documentation_is_allowed_by_name() {
        let planned: Vec<String> = ALWAYS_LOCAL
            .iter()
            .enumerate()
            .map(|(index, name)| path(index + 1, name))
            .collect();

        assert!(
            classify(&planned, &shown(&[])).source.is_empty(),
            "a clean store plans these and no cache can serve them"
        );
    }

    #[test]
    fn a_package_planned_beside_the_documentation_is_still_refused() {
        let planned = vec![
            path(1, "options.json"),
            path(2, "hm-modules-messages"),
            path(3, "hello-2.12.3"),
        ];

        assert_eq!(
            names(&classify(&planned, &shown(&[])).source),
            ["hello-2.12.3"]
        );
    }

    #[test]
    fn the_allowed_names_are_matched_whole() {
        assert!(is_always_local("options.json"));
        assert!(!is_always_local("options.json-wrapper"));
        assert!(!is_always_local("options"));
        assert!(!is_always_local("home-configuration-reference-manpage-1.0"));
    }

    #[test]
    fn a_plan_of_borrowed_paths_classifies_the_same_way() {
        let planned = ["/nix/store/00000000000000000000000000000001-hello-2.12.3.drv"];

        assert_eq!(
            names(&classify(&planned, &shown(&[])).source),
            ["hello-2.12.3"]
        );
    }
}
