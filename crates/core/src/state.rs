use std::collections::HashSet;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

static SEED_RENDERED: LazyLock<String> = LazyLock::new(|| StateManifest::seed().render());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateManifest {
    pub version: u32,
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagePartition<'a> {
    pub installed: Vec<&'a str>,
    pub missing: Vec<&'a str>,
}

impl StateManifest {
    pub fn seed() -> Self {
        Self {
            version: 1,
            packages: vec!["git".to_string()],
        }
    }

    pub fn seed_rendered() -> &'static str {
        &SEED_RENDERED
    }

    pub fn render(&self) -> String {
        format!(
            "{}\n",
            serde_json::to_string_pretty(self).expect("StateManifest always serializes")
        )
    }

    pub fn parse(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    pub fn partition<'a, S: AsRef<str>>(&self, requested: &'a [S]) -> PackagePartition<'a> {
        let installed_set: HashSet<&str> = self.packages.iter().map(String::as_str).collect();
        let mut seen: HashSet<&str> = HashSet::with_capacity(requested.len());
        let mut installed = Vec::new();
        let mut missing = Vec::new();

        for package in requested.iter().map(AsRef::as_ref) {
            if !seen.insert(package) {
                continue;
            }
            if installed_set.contains(package) {
                installed.push(package);
            } else {
                missing.push(package);
            }
        }

        PackagePartition { installed, missing }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_is_versioned_and_starts_with_git() {
        let seed = StateManifest::seed();
        assert_eq!(seed.version, 1);
        assert_eq!(seed.packages, vec!["git".to_string()]);
    }

    #[test]
    fn seed_rendered_matches_rendering_the_seed() {
        assert_eq!(
            StateManifest::seed_rendered(),
            StateManifest::seed().render()
        );
    }

    #[test]
    fn seed_rendered_hands_out_the_same_buffer_every_time() {
        assert!(std::ptr::eq(
            StateManifest::seed_rendered(),
            StateManifest::seed_rendered()
        ));
    }

    #[test]
    fn render_ends_with_a_trailing_newline() {
        assert!(StateManifest::seed().render().ends_with('\n'));
    }

    #[test]
    fn render_then_parse_round_trips() {
        let manifest = StateManifest {
            version: 1,
            packages: vec!["git".to_string(), "ripgrep".to_string()],
        };
        assert_eq!(StateManifest::parse(&manifest.render()).unwrap(), manifest);
    }

    #[test]
    fn parse_defaults_packages_when_the_field_is_absent() {
        let manifest = StateManifest::parse(r#"{"version": 1}"#).unwrap();
        assert_eq!(manifest.packages, Vec::<String>::new());
    }

    #[test]
    fn parse_ignores_unknown_fields_for_forward_compatibility() {
        let manifest =
            StateManifest::parse(r#"{"version": 1, "packages": ["git"], "programs": {}}"#).unwrap();
        assert_eq!(manifest.packages, vec!["git".to_string()]);
    }

    #[test]
    fn parse_rejects_malformed_json() {
        assert!(StateManifest::parse("not json").is_err());
    }

    fn manifest(packages: &[&str]) -> StateManifest {
        StateManifest {
            version: 1,
            packages: packages.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn partition_splits_requested_packages_around_the_installed_set() {
        let manifest = manifest(&["git", "ripgrep"]);
        let requested = ["ripgrep".to_string(), "fd".to_string(), "git".to_string()];

        assert_eq!(
            manifest.partition(&requested),
            PackagePartition {
                installed: vec!["ripgrep", "git"],
                missing: vec!["fd"],
            }
        );
    }

    #[test]
    fn partition_keeps_the_first_occurrence_order_and_drops_duplicates() {
        let manifest = manifest(&["git"]);
        let requested = [
            "fd".to_string(),
            "git".to_string(),
            "fd".to_string(),
            "bat".to_string(),
            "git".to_string(),
        ];

        let partition = manifest.partition(&requested);
        assert_eq!(partition.installed, vec!["git"]);
        assert_eq!(partition.missing, vec!["fd", "bat"]);
    }

    #[test]
    fn partition_is_a_partition_of_the_deduped_request() {
        let manifest = manifest(&["git", "ripgrep"]);
        let requested = [
            "git".to_string(),
            "fd".to_string(),
            "git".to_string(),
            "bat".to_string(),
        ];

        let partition = manifest.partition(&requested);
        let installed: HashSet<&str> = partition.installed.iter().copied().collect();
        let missing: HashSet<&str> = partition.missing.iter().copied().collect();
        let deduped: HashSet<&str> = requested.iter().map(String::as_str).collect();

        assert!(installed.is_disjoint(&missing));
        assert_eq!(&installed | &missing, deduped);
        assert_eq!(partition.installed.len() + partition.missing.len(), 3);
    }

    #[test]
    fn partition_of_an_empty_request_is_empty() {
        assert_eq!(
            manifest(&["git"]).partition::<String>(&[]),
            PackagePartition {
                installed: Vec::new(),
                missing: Vec::new(),
            }
        );
    }

    #[test]
    fn partition_against_an_empty_manifest_leaves_everything_missing() {
        let requested = ["git".to_string(), "fd".to_string()];
        let partition = manifest(&[]).partition(&requested);

        assert!(partition.installed.is_empty());
        assert_eq!(partition.missing, vec!["git", "fd"]);
    }
}
