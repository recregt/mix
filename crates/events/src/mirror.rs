#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("the mirror must be an http or https URL without spaces: {0:?}")]
    Url(String),

    #[error("the mirror key must be a single <name>:<key> entry: {0:?}")]
    Key(String),

    #[error("a mirror key was given without a mirror")]
    KeyWithoutMirror,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirror {
    url: String,
    key: Option<String>,
}

impl Mirror {
    pub fn given(url: Option<&str>, key: Option<&str>) -> Result<Option<Self>, Invalid> {
        let url = url.map(str::trim).filter(|url| !url.is_empty());
        let key = key.map(str::trim).filter(|key| !key.is_empty());
        match (url, key) {
            (Some(url), key) => Self::new(url, key).map(Some),
            (None, Some(_)) => Err(Invalid::KeyWithoutMirror),
            (None, None) => Ok(None),
        }
    }

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

fn is_binary_cache_key(key: &str) -> bool {
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

    #[test]
    fn nothing_can_be_smuggled_through_the_mirror() {
        for url in [
            "https://mirror\ntrusted-users = root evil",
            "https://mirror trusted-users",
            "https://mirror#comment",
            "ftp://mirror",
            "https://",
            "mirror.internal",
        ] {
            assert_eq!(
                Mirror::given(Some(url), None),
                Err(Invalid::Url(url.trim().trim_end_matches('/').to_string())),
                "{url:?}"
            );
        }
    }

    #[test]
    fn nothing_can_be_smuggled_through_the_key() {
        for key in [
            "name:key\ntrusted-users = evil",
            "name key",
            "no-colon",
            "a:b:c",
        ] {
            assert!(
                matches!(
                    Mirror::given(Some("https://mirror"), Some(key)),
                    Err(Invalid::Key(_))
                ),
                "{key:?}"
            );
        }
    }

    #[test]
    fn a_key_needs_a_mirror() {
        assert_eq!(
            Mirror::given(None, Some("mix-mirror:AAAA")),
            Err(Invalid::KeyWithoutMirror)
        );
        assert_eq!(Mirror::given(Some(" "), None), Ok(None));
    }
}
