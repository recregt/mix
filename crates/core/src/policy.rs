use serde::{Deserialize, Serialize};

pub const FORMAT: u32 = 1;

pub const CACHE_NIXOS_ORG_KEY: &str =
    "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("the mirror must be an http or https URL without spaces: {0:?}")]
    Url(String),

    #[error("the mirror key must be a single <name>:<key> entry: {0:?}")]
    Key(String),

    #[error("a mirror key was given without a mirror")]
    KeyWithoutMirror,

    #[error("the policy was written by a newer version of mix (format {0})")]
    Newer(u32),

    #[error("the policy is not valid JSON: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirror {
    url: String,
    key: Option<String>,
}

impl Mirror {
    pub fn new(url: &str, key: Option<&str>) -> Result<Self, Invalid> {
        let url = url.trim().trim_end_matches('/');
        if !is_mirror_url(url) {
            return Err(Invalid::Url(url.to_string()));
        }
        let key = match key.map(str::trim).filter(|key| !key.is_empty()) {
            Some(key) if is_binary_cache_key(key) => Some(key.to_string()),
            Some(key) => return Err(Invalid::Key(key.to_string())),
            None => None,
        };
        Ok(Self {
            url: url.to_string(),
            key,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    pub fn file(&self, name: &str) -> String {
        format!("{}/{name}", self.url)
    }

    pub fn cache(&self) -> String {
        self.file("cache")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    mirror: Option<Mirror>,
    rendered: String,
    nix_conf: String,
}

impl Default for Policy {
    fn default() -> Self {
        Self::with(None)
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    format: u32,
    mirror: Option<StoredMirror>,
}

#[derive(Serialize, Deserialize)]
struct StoredMirror {
    url: String,
    key: Option<String>,
}

impl Policy {
    pub fn new(url: Option<&str>, key: Option<&str>) -> Result<Self, Invalid> {
        let url = url.map(str::trim).filter(|url| !url.is_empty());
        let key = key.map(str::trim).filter(|key| !key.is_empty());
        match (url, key) {
            (Some(url), key) => Ok(Self::with(Some(Mirror::new(url, key)?))),
            (None, Some(_)) => Err(Invalid::KeyWithoutMirror),
            (None, None) => Ok(Self::default()),
        }
    }

    pub fn mirror(&self) -> Option<&Mirror> {
        self.mirror.as_ref()
    }

    pub fn parse(contents: &str) -> Result<Self, Invalid> {
        let stored: Stored =
            serde_json::from_str(contents).map_err(|e| Invalid::Malformed(e.to_string()))?;
        if stored.format > FORMAT {
            return Err(Invalid::Newer(stored.format));
        }
        match stored.mirror {
            Some(mirror) => Self::new(Some(&mirror.url), mirror.key.as_deref()),
            None => Ok(Self::default()),
        }
    }

    pub fn load(contents: Option<&str>) -> Self {
        contents
            .and_then(|contents| Self::parse(contents).ok())
            .unwrap_or_default()
    }

    pub fn render(&self) -> &str {
        &self.rendered
    }

    pub fn nix_conf(&self) -> &str {
        &self.nix_conf
    }

    fn with(mirror: Option<Mirror>) -> Self {
        Self {
            rendered: render(mirror.as_ref()),
            nix_conf: nix_conf(mirror.as_ref()),
            mirror,
        }
    }
}

fn render(mirror: Option<&Mirror>) -> String {
    let stored = Stored {
        format: FORMAT,
        mirror: mirror.map(|mirror| StoredMirror {
            url: mirror.url.clone(),
            key: mirror.key.clone(),
        }),
    };
    let mut rendered = serde_json::to_string_pretty(&stored).expect("a policy always serialises");
    rendered.push('\n');
    rendered
}

fn nix_conf(mirror: Option<&Mirror>) -> String {
    let mut conf = String::from(
        "build-users-group = nixbld\nexperimental-features = nix-command flakes\ntrusted-users = root\n",
    );
    if let Some(mirror) = mirror {
        conf.push_str(&format!("substituters = {}\n", mirror.cache()));
        if let Some(key) = &mirror.key {
            conf.push_str(&format!(
                "trusted-public-keys = {CACHE_NIXOS_ORG_KEY} {key}\n"
            ));
        }
    }
    conf
}

fn is_mirror_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|rest| !rest.is_empty())
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '#' || c == '"')
}

pub fn is_binary_cache_key(key: &str) -> bool {
    let Some((name, material)) = key.split_once(':') else {
        return false;
    };
    !name.is_empty()
        && !material.is_empty()
        && !material.contains(':')
        && !key
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '#')
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "mix-mirror:AAAA";

    #[test]
    fn the_default_trusts_root_alone_and_adds_no_substituter() {
        assert_eq!(
            Policy::default().nix_conf(),
            "build-users-group = nixbld\nexperimental-features = nix-command flakes\ntrusted-users = root\n"
        );
    }

    #[test]
    fn a_mirror_with_a_key_becomes_the_substituter_next_to_the_upstream_key() {
        let policy = Policy::new(Some("https://mirror.internal/"), Some(KEY)).unwrap();

        let conf = policy.nix_conf();

        assert!(conf.contains("trusted-users = root\n"));
        assert!(conf.contains("substituters = https://mirror.internal/cache\n"));
        assert!(conf.contains(&format!(
            "trusted-public-keys = {CACHE_NIXOS_ORG_KEY} {KEY}\n"
        )));
    }

    #[test]
    fn a_mirror_without_a_key_adds_no_key() {
        let policy = Policy::new(Some("http://mirror.internal"), None).unwrap();
        let conf = policy.nix_conf();

        assert!(conf.contains("substituters = http://mirror.internal/cache\n"));
        assert!(!conf.contains("trusted-public-keys"));
    }

    #[test]
    fn nothing_can_be_smuggled_into_nix_conf_through_the_mirror() {
        for url in [
            "https://mirror\ntrusted-users = root evil",
            "https://mirror trusted-users",
            "https://mirror#comment",
            "ftp://mirror",
            "https://",
            "mirror.internal",
        ] {
            assert_eq!(
                Policy::new(Some(url), None),
                Err(Invalid::Url(url.trim().trim_end_matches('/').to_string())),
                "{url:?}"
            );
        }
    }

    #[test]
    fn nothing_can_be_smuggled_into_nix_conf_through_the_key() {
        for key in [
            "name:key\ntrusted-users = evil",
            "name key",
            "no-colon",
            "a:b:c",
        ] {
            assert!(
                matches!(
                    Policy::new(Some("https://mirror"), Some(key)),
                    Err(Invalid::Key(_))
                ),
                "{key:?}"
            );
        }
    }

    #[test]
    fn a_key_needs_a_mirror() {
        assert_eq!(Policy::new(None, Some(KEY)), Err(Invalid::KeyWithoutMirror));
    }

    #[test]
    fn a_rendered_policy_parses_back_to_itself() {
        for policy in [
            Policy::default(),
            Policy::new(Some("https://mirror.internal"), None).unwrap(),
            Policy::new(Some("https://mirror.internal"), Some(KEY)).unwrap(),
        ] {
            assert_eq!(Policy::parse(policy.render()), Ok(policy));
        }
    }

    #[test]
    fn a_stored_policy_is_validated_like_a_given_one() {
        let smuggled =
            r#"{"format":1,"mirror":{"url":"https://m\ntrusted-users = evil","key":null}}"#;

        assert!(matches!(Policy::parse(smuggled), Err(Invalid::Url(_))));
        assert_eq!(Policy::load(Some(smuggled)), Policy::default());
    }

    #[test]
    fn a_missing_or_unreadable_policy_falls_back_to_the_default() {
        assert_eq!(Policy::load(None), Policy::default());
        assert_eq!(Policy::load(Some("{broken")), Policy::default());
        assert_eq!(
            Policy::load(Some(r#"{"format":99,"mirror":null}"#)),
            Policy::default()
        );
    }
}
