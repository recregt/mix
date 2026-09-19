//! Where nix is fetched from: the archive, the flake inputs, and the binary cache.

use std::time::Duration;

use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

const CACHE_NIXOS_ORG_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
const TRUSTED_KEY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const TRUSTED_KEY_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

pub fn filter_mirror(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

pub fn mirror_url(base: &str, filename: &str) -> String {
    format!("{}/{filename}", base.trim().trim_end_matches('/'))
}

pub fn nixpkgs_override(base: &str) -> String {
    format!(
        "tarball+{}",
        mirror_url(base, &format!("nixpkgs-{NIXPKGS_REV}.tar.gz"))
    )
}

pub fn home_manager_override(base: &str) -> String {
    format!(
        "tarball+{}",
        mirror_url(base, &format!("home-manager-{HOME_MANAGER_REV}.tar.gz"))
    )
}

pub fn substituter(base: &str) -> String {
    mirror_url(base, "cache")
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

fn serves_https(base: &str) -> bool {
    base.trim()
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
}

async fn fetch_mirror_key(base: &str) -> Option<String> {
    let url = mirror_url(base, "cache/mix-mirror.pub");
    let client = reqwest::Client::builder()
        .connect_timeout(TRUSTED_KEY_CONNECT_TIMEOUT)
        .timeout(TRUSTED_KEY_FETCH_TIMEOUT)
        .build()
        .ok()?;
    let response = client.get(&url).send().await.ok()?;
    let body = response.error_for_status().ok()?.text().await.ok()?;
    let key = body.trim().to_string();
    if !is_binary_cache_key(&key) {
        tracing::warn!("ignoring the key served by {url}: not a single <name>:<key> entry");
        return None;
    }
    Some(key)
}

async fn mirror_key(base: &str, configured: Option<&str>) -> Option<String> {
    if let Some(key) = filter_mirror(configured) {
        if is_binary_cache_key(key) {
            return Some(key.to_string());
        }
        tracing::warn!("ignoring the configured mirror key: not a single <name>:<key> entry");
        return None;
    }
    if !serves_https(base) {
        tracing::warn!(
            "not fetching a signing key over plain HTTP: pass --mirror-key to trust {base}"
        );
        return None;
    }
    fetch_mirror_key(base).await
}

pub async fn trusted_public_keys(base: &str, configured_key: Option<&str>) -> String {
    match mirror_key(base, configured_key).await {
        Some(key) => format!("{CACHE_NIXOS_ORG_KEY} {key}"),
        None => CACHE_NIXOS_ORG_KEY.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use mockito::{Mock, Server, ServerGuard};

    #[test]
    fn mirror_url_joins_base_and_filename() {
        assert_eq!(
            mirror_url("http://mirror.internal", "nix-2.35.2-x86_64-linux.tar.xz"),
            "http://mirror.internal/nix-2.35.2-x86_64-linux.tar.xz"
        );
    }

    #[test]
    fn mirror_url_trims_a_trailing_slash_on_the_base() {
        assert_eq!(
            mirror_url("http://mirror.internal/", "nix-2.35.2-x86_64-linux.tar.xz"),
            "http://mirror.internal/nix-2.35.2-x86_64-linux.tar.xz"
        );
    }

    #[test]
    fn mirror_url_trims_surrounding_whitespace_on_the_base() {
        assert_eq!(
            mirror_url(
                " http://mirror.internal/ ",
                "nix-2.35.2-x86_64-linux.tar.xz"
            ),
            "http://mirror.internal/nix-2.35.2-x86_64-linux.tar.xz"
        );
    }

    #[test]
    fn filter_mirror_none_when_unset() {
        assert_eq!(filter_mirror(None), None);
    }

    #[test]
    fn filter_mirror_none_when_empty_string() {
        assert_eq!(filter_mirror(Some("")), None);
    }

    #[test]
    fn filter_mirror_none_when_whitespace_only() {
        assert_eq!(filter_mirror(Some("   ")), None);
    }

    #[test]
    fn filter_mirror_some_when_a_real_url_is_set() {
        assert_eq!(
            filter_mirror(Some("http://mirror.internal")),
            Some("http://mirror.internal")
        );
    }

    #[test]
    fn nixpkgs_override_uses_a_tarball_scheme_keyed_by_the_pinned_revision() {
        assert_eq!(
            nixpkgs_override("http://mirror.internal"),
            format!("tarball+http://mirror.internal/nixpkgs-{NIXPKGS_REV}.tar.gz")
        );
    }

    #[test]
    fn home_manager_override_uses_a_tarball_scheme_keyed_by_the_pinned_revision() {
        assert_eq!(
            home_manager_override("http://mirror.internal"),
            format!("tarball+http://mirror.internal/home-manager-{HOME_MANAGER_REV}.tar.gz")
        );
    }

    #[test]
    fn substituter_points_at_a_cache_path_under_the_mirror() {
        assert_eq!(
            substituter("http://mirror.internal"),
            "http://mirror.internal/cache"
        );
    }

    const MIRROR_KEY: &str = "mix-mirror-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const KEY_PATH: &str = "/cache/mix-mirror.pub";

    async fn mirror_serving(status: usize, body: &str) -> (ServerGuard, Mock) {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", KEY_PATH)
            .with_status(status)
            .with_body(body)
            .create_async()
            .await;
        (server, mock)
    }

    #[test]
    fn is_binary_cache_key_accepts_a_single_entry() {
        assert!(is_binary_cache_key(MIRROR_KEY));
        assert!(is_binary_cache_key(CACHE_NIXOS_ORG_KEY));
    }

    #[test]
    fn is_binary_cache_key_rejects_a_second_key() {
        assert!(!is_binary_cache_key(&format!(
            "{MIRROR_KEY} evil-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        )));
        assert!(!is_binary_cache_key(&format!("{MIRROR_KEY}\nevil-1:AAAA=")));
    }

    #[test]
    fn is_binary_cache_key_rejects_anything_that_is_not_name_colon_key() {
        assert!(!is_binary_cache_key(""));
        assert!(!is_binary_cache_key("no-colon"));
        assert!(!is_binary_cache_key(":AAAA="));
        assert!(!is_binary_cache_key("name:"));
        assert!(!is_binary_cache_key("name:AAAA=:extra"));
        assert!(!is_binary_cache_key("name:AAAA=#comment"));
    }

    #[test]
    fn serves_https_only_for_an_https_base() {
        assert!(serves_https("https://mirror.internal"));
        assert!(serves_https(" HTTPS://mirror.internal "));
        assert!(!serves_https("http://mirror.internal"));
        assert!(!serves_https("mirror.internal"));
        assert!(!serves_https("https:/"));
    }

    #[tokio::test]
    async fn trusted_public_keys_trusts_a_key_supplied_out_of_band() {
        let (server, mock) = mirror_serving(200, "attacker-1:BBBBBBBBBBBBBBBBBBBBBBBBBBBB=").await;

        let keys = trusted_public_keys(&server.url(), Some(MIRROR_KEY)).await;

        assert_eq!(keys, format!("{CACHE_NIXOS_ORG_KEY} {MIRROR_KEY}"));
        assert!(
            !mock.matched_async().await,
            "a configured key must not be overridden by one the mirror serves"
        );
    }

    #[tokio::test]
    async fn trusted_public_keys_ignores_a_malformed_configured_key() {
        assert_eq!(
            trusted_public_keys("https://mirror.internal", Some("not-a-key")).await,
            CACHE_NIXOS_ORG_KEY
        );
    }

    #[tokio::test]
    async fn trusted_public_keys_ignores_a_configured_key_smuggling_a_second_one() {
        let smuggled = format!("{MIRROR_KEY} attacker-1:BBBB=");
        assert_eq!(
            trusted_public_keys("https://mirror.internal", Some(&smuggled)).await,
            CACHE_NIXOS_ORG_KEY
        );
    }

    #[tokio::test]
    async fn trusted_public_keys_never_fetches_a_key_over_plain_http() {
        let (server, mock) = mirror_serving(200, MIRROR_KEY).await;

        let keys = trusted_public_keys(&server.url(), None).await;

        assert_eq!(keys, CACHE_NIXOS_ORG_KEY);
        assert!(
            !mock.matched_async().await,
            "a plain-HTTP mirror must not be asked for the key that authorises it"
        );
    }

    #[tokio::test]
    async fn trusted_public_keys_is_just_the_default_when_the_mirror_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let keys = trusted_public_keys(&format!("https://{addr}"), None).await;

        assert_eq!(keys, CACHE_NIXOS_ORG_KEY);
    }

    #[tokio::test]
    async fn fetch_mirror_key_reads_the_key_the_mirror_publishes() {
        let (server, mock) = mirror_serving(200, &format!("{MIRROR_KEY}\n")).await;

        assert_eq!(
            fetch_mirror_key(&server.url()).await.as_deref(),
            Some(MIRROR_KEY)
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn fetch_mirror_key_reads_it_from_under_the_mirror_cache_path() {
        let (server, mock) = mirror_serving(200, MIRROR_KEY).await;

        fetch_mirror_key(&format!("{}/", server.url())).await;

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn fetch_mirror_key_ignores_a_mirror_that_does_not_publish_a_key() {
        let (server, _mock) = mirror_serving(404, "<html>not found</html>").await;

        assert_eq!(fetch_mirror_key(&server.url()).await, None);
    }

    #[tokio::test]
    async fn fetch_mirror_key_ignores_a_mirror_that_errors() {
        let (server, _mock) = mirror_serving(500, "boom").await;

        assert_eq!(fetch_mirror_key(&server.url()).await, None);
    }

    #[tokio::test]
    async fn fetch_mirror_key_ignores_an_empty_key_file() {
        let (server, _mock) = mirror_serving(200, "").await;

        assert_eq!(fetch_mirror_key(&server.url()).await, None);
    }

    #[tokio::test]
    async fn fetch_mirror_key_ignores_a_blank_key_file() {
        let (server, _mock) = mirror_serving(200, "  \n\t\n").await;

        assert_eq!(fetch_mirror_key(&server.url()).await, None);
    }

    #[tokio::test]
    async fn fetch_mirror_key_ignores_a_key_file_holding_several_keys() {
        let (server, _mock) = mirror_serving(200, &format!("{MIRROR_KEY} attacker-1:BBBB=")).await;

        assert_eq!(fetch_mirror_key(&server.url()).await, None);
    }

    #[tokio::test]
    async fn trusted_public_keys_keeps_the_default_cache_key_first() {
        let keys = trusted_public_keys("http://mirror.internal", Some(MIRROR_KEY)).await;

        assert!(keys.starts_with(CACHE_NIXOS_ORG_KEY));
        assert_eq!(keys.split(' ').count(), 2);
    }
}
