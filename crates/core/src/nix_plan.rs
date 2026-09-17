//! Reading the build plan `nix build --dry-run` prints, so a package that is missing from the
//! binary cache can be refused before nix starts compiling it.
//!
//! nix reports the plan as two lists: the derivations it would build and the paths it would
//! fetch. Only the first matters here, and only partly: home-manager always builds its own
//! generation locally — the profile is assembled on the machine it is for — so those entries are
//! expected and have to be told apart from a package that would really be compiled from source.
//!
//! The plan is read in a single pass over the output and nothing is copied except the store
//! paths themselves, and the derivations are classified straight out of `nix derivation show`
//! without deserialising the attributes that the answer does not depend on.

use std::collections::HashMap;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// Header nix prints before the list of derivations it would build.
const WILL_BUILD_SUFFIX: &str = "derivations will be built:";
const WILL_BUILD_ONE: &str = "this derivation will be built:";

/// Indentation nix puts in front of every path it lists under a header.
const ENTRY_INDENT: &str = "  ";

const STORE_PREFIX: &str = "/nix/store/";
const DRV_SUFFIX: &str = ".drv";

/// Length of the hash nix puts at the front of a store path name, plus its dash.
const HASH_PREFIX_LEN: usize = 33;

/// Which list of the plan the parser is currently reading.
#[derive(Debug, PartialEq, Eq)]
enum Section {
    WillBuild,
    Other,
}

/// The derivations `nix build --dry-run` says it would build, in the order nix listed them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildPlan {
    to_build: Vec<String>,
}

impl BuildPlan {
    /// Reads a plan out of what a dry run wrote to stderr.
    ///
    /// Anything that is not an entry of the "will be built" list is ignored, so warnings and the
    /// list of paths to fetch flow past without being mistaken for work.
    pub fn parse(output: &str) -> Self {
        let mut to_build = Vec::new();
        let mut section = Section::Other;

        for line in output.lines() {
            if let Some(entry) = line.strip_prefix(ENTRY_INDENT) {
                if section == Section::WillBuild && is_derivation_path(entry) {
                    to_build.push(entry.to_string());
                }
                continue;
            }
            section = if announces_builds(line) {
                Section::WillBuild
            } else {
                Section::Other
            };
        }

        Self { to_build }
    }

    pub fn to_build(&self) -> &[String] {
        &self.to_build
    }

    pub fn is_empty(&self) -> bool {
        self.to_build.is_empty()
    }
}

fn announces_builds(line: &str) -> bool {
    let line = line.trim_end();
    line == WILL_BUILD_ONE || line.ends_with(WILL_BUILD_SUFFIX)
}

fn is_derivation_path(entry: &str) -> bool {
    entry.starts_with(STORE_PREFIX) && entry.ends_with(DRV_SUFFIX)
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

/// The planned derivations that would really be compiled, named for a human.
///
/// A derivation nix would have built locally even with a warm cache — home-manager's generation,
/// the profile it links together, the small files it renders — is not a source build: it is how a
/// home-manager profile is assembled, and no binary cache can ever hold it. Those are recognised
/// by the attributes nix itself uses to decide as much, `preferLocalBuild` and
/// `allowSubstitutes`, rather than by name, so a renamed or newly added one still passes.
///
/// `derivations` is the output of `nix derivation show` for exactly `planned`. If it cannot be
/// read, every planned derivation is reported: refusing to build is the safe answer.
pub fn source_builds<'a>(planned: &'a [String], derivations: &str) -> Vec<&'a str> {
    let shown = Shown::parse(derivations);

    planned
        .iter()
        .filter(|path| !shown.is_local(path))
        .map(|path| derivation_name(path))
        .collect()
}

/// What `nix derivation show` said about each derivation, keyed the way nix keyed it.
struct Shown<'a> {
    derivations: HashMap<&'a str, Derivation>,
}

impl<'a> Shown<'a> {
    fn parse(raw: &'a str) -> Self {
        // nix 2.35 wraps the map in a versioned document; older releases wrote the bare map.
        let derivations = match serde_json::from_str::<Document>(raw) {
            Ok(document) if !document.derivations.is_empty() => document.derivations,
            _ => serde_json::from_str(raw).unwrap_or_default(),
        };
        Self { derivations }
    }

    /// Whether nix would have built this derivation locally no matter what the cache holds.
    fn is_local(&self, path: &str) -> bool {
        let key = path.rsplit('/').next().unwrap_or(path);
        self.derivations
            .get(key)
            .or_else(|| self.derivations.get(path))
            .is_some_and(Derivation::is_local)
    }
}

#[derive(Deserialize)]
struct Document<'a> {
    #[serde(borrow, default)]
    derivations: HashMap<&'a str, Derivation>,
}

/// One derivation, reduced to the two attributes that decide where it is built.
///
/// Every other attribute is skipped without being deserialised, which matters: the attribute set
/// of a single derivation is regularly tens of kilobytes.
#[derive(Deserialize)]
struct Derivation {
    #[serde(default)]
    env: BuildFlags,
    // Derivations built with `__structuredAttrs` carry their attributes here instead.
    #[serde(default, rename = "structuredAttrs")]
    structured_attrs: Option<BuildFlags>,
}

impl Derivation {
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

/// The two attributes, wherever nix chose to write them.
#[derive(Default)]
struct BuildFlags {
    prefer_local_build: Option<bool>,
    allow_substitutes: Option<bool>,
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

    const PLAN: &str = "\
these 2 derivations will be built:
  /nix/store/00000000000000000000000000000001-home-manager-path.drv
  /nix/store/00000000000000000000000000000002-hello-2.12.3.drv
these 2 paths will be fetched (1.5 MiB download, 4.0 MiB unpacked):
  /nix/store/00000000000000000000000000000003-fd-10.5.0
  /nix/store/00000000000000000000000000000004-git-2.51.0
";

    #[test]
    fn a_plan_lists_only_the_derivations_that_would_be_built() {
        let plan = BuildPlan::parse(PLAN);
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
        assert!(BuildPlan::parse("").is_empty());
        assert!(
            BuildPlan::parse("these 2 paths will be fetched (1.5 MiB download):\n  /nix/store/x\n")
                .is_empty()
        );
    }

    #[test]
    fn a_single_planned_derivation_is_announced_in_the_singular() {
        let plan = BuildPlan::parse(
            "this derivation will be built:\n  /nix/store/00000000000000000000000000000001-a.drv\n",
        );
        assert_eq!(plan.to_build().len(), 1);
    }

    #[test]
    fn a_warning_between_the_lists_does_not_swallow_the_plan() {
        let plan = BuildPlan::parse(
            "warning: Git tree is dirty\nthis derivation will be built:\n  \
             /nix/store/00000000000000000000000000000001-a.drv\nwarning: still dirty\n  \
             /nix/store/00000000000000000000000000000002-b.drv\n",
        );
        assert_eq!(plan.to_build().len(), 1);
    }

    #[test]
    fn a_line_that_is_not_a_derivation_path_is_not_a_planned_build() {
        let plan = BuildPlan::parse(
            "this derivation will be built:\n  /nix/store/00000000000000000000000000000001-a\n  \
             not-a-store-path.drv\n",
        );
        assert!(plan.is_empty());
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

    fn shown(entries: &[(&str, &str)]) -> String {
        let body: Vec<String> = entries
            .iter()
            .map(|(name, attrs)| format!("\"{name}\":{attrs}"))
            .collect();
        format!("{{\"derivations\":{{{}}},\"version\":4}}", body.join(","))
    }

    #[test]
    fn a_derivation_that_prefers_a_local_build_is_not_a_source_build() {
        let planned = vec![
            "/nix/store/00000000000000000000000000000001-home-manager-generation.drv".to_string(),
        ];
        let derivations = shown(&[(
            "00000000000000000000000000000001-home-manager-generation.drv",
            r#"{"env":{"name":"home-manager-generation","preferLocalBuild":"1"}}"#,
        )]);

        assert!(source_builds(&planned, &derivations).is_empty());
    }

    #[test]
    fn a_derivation_that_refuses_substitutes_is_not_a_source_build() {
        let planned =
            vec!["/nix/store/00000000000000000000000000000001-home-manager-files.drv".to_string()];
        let derivations = shown(&[(
            "00000000000000000000000000000001-home-manager-files.drv",
            r#"{"env":{"allowSubstitutes":""}}"#,
        )]);

        assert!(source_builds(&planned, &derivations).is_empty());
    }

    #[test]
    fn structured_attributes_are_read_as_booleans() {
        let planned =
            vec!["/nix/store/00000000000000000000000000000001-home-manager-path.drv".to_string()];
        let derivations = shown(&[(
            "00000000000000000000000000000001-home-manager-path.drv",
            r#"{"env":{"out":"/nix/store/x"},"structuredAttrs":{"preferLocalBuild":true,"allowSubstitutes":false,"buildInputs":["a","b"]}}"#,
        )]);

        assert!(source_builds(&planned, &derivations).is_empty());
    }

    #[test]
    fn a_package_that_would_be_compiled_is_reported_by_name() {
        let planned = vec![
            "/nix/store/00000000000000000000000000000001-home-manager-generation.drv".to_string(),
            "/nix/store/00000000000000000000000000000002-hello-2.12.3.drv".to_string(),
        ];
        let derivations = shown(&[
            (
                "00000000000000000000000000000001-home-manager-generation.drv",
                r#"{"env":{"preferLocalBuild":"1"}}"#,
            ),
            (
                "00000000000000000000000000000002-hello-2.12.3.drv",
                r#"{"env":{"name":"hello"},"structuredAttrs":{"doCheck":true}}"#,
            ),
        ]);

        assert_eq!(source_builds(&planned, &derivations), ["hello-2.12.3"]);
    }

    #[test]
    fn a_derivation_nix_said_nothing_about_is_treated_as_a_source_build() {
        let planned =
            vec!["/nix/store/00000000000000000000000000000001-hello-2.12.3.drv".to_string()];

        assert_eq!(
            source_builds(&planned, &shown(&[])),
            ["hello-2.12.3"],
            "a derivation that cannot be classified must not be built silently"
        );
    }

    #[test]
    fn an_unreadable_document_leaves_every_planned_derivation_refused() {
        let planned =
            vec!["/nix/store/00000000000000000000000000000001-hello-2.12.3.drv".to_string()];

        assert_eq!(source_builds(&planned, "not json at all"), ["hello-2.12.3"]);
    }

    #[test]
    fn a_bare_map_of_derivations_is_still_understood() {
        let planned =
            vec!["/nix/store/00000000000000000000000000000001-home-manager-path.drv".to_string()];
        let derivations = r#"{"/nix/store/00000000000000000000000000000001-home-manager-path.drv":{"env":{"preferLocalBuild":"1"}}}"#;

        assert!(source_builds(&planned, derivations).is_empty());
    }
}
