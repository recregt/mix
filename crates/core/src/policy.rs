use serde::{Deserialize, Serialize};

pub const FORMAT: u32 = 1;

pub const CACHE_NIXOS_ORG_KEY: &str =
    "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

pub use mix_events::mirror::Mirror;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error(transparent)]
    Mirror(#[from] mix_events::mirror::Invalid),

    #[error("the policy was written by a newer version of mix (format {0})")]
    Newer(u32),

    #[error("the policy is not valid JSON: {0}")]
    Malformed(String),
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
        Ok(Self::with(Mirror::given(url, key)?))
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
            url: mirror.url().to_string(),
            key: mirror.key().map(str::to_string),
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
        conf.push_str(&format!("substituters = {}/cache\n", mirror.url()));
        if let Some(key) = mirror.key() {
            conf.push_str(&format!(
                "trusted-public-keys = {CACHE_NIXOS_ORG_KEY} {key}\n"
            ));
        }
    }
    conf
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
    fn a_mirror_is_checked_before_it_reaches_nix_conf() {
        assert!(matches!(
            Policy::new(Some("https://mirror\ntrusted-users = evil"), None),
            Err(Invalid::Mirror(mix_events::mirror::Invalid::Url(_)))
        ));
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

        assert!(matches!(
            Policy::parse(smuggled),
            Err(Invalid::Mirror(mix_events::mirror::Invalid::Url(_)))
        ));
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
