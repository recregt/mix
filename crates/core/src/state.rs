use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateManifest {
    pub version: u32,
    #[serde(default)]
    pub packages: Vec<String>,
}

impl StateManifest {
    pub fn seed() -> Self {
        Self {
            version: 1,
            packages: vec!["git".to_string()],
        }
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
}
