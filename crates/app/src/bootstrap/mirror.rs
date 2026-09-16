use std::time::Duration;

use crate::bootstrap::pins::{HOME_MANAGER_REV, NIXPKGS_REV};

const CACHE_NIXOS_ORG_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
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

async fn fetch_mirror_key(base: &str) -> Option<String> {
    let url = mirror_url(base, "cache/mix-mirror.pub");
    let client = reqwest::Client::builder()
        .timeout(TRUSTED_KEY_FETCH_TIMEOUT)
        .build()
        .ok()?;
    let response = client.get(&url).send().await.ok()?;
    let body = response.error_for_status().ok()?.text().await.ok()?;
    let key = body.trim().to_string();
    (!key.is_empty()).then_some(key)
}

pub async fn trusted_public_keys(base: &str) -> String {
    match fetch_mirror_key(base).await {
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

    #[tokio::test]
    async fn trusted_public_keys_is_just_the_default_when_the_mirror_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let keys = trusted_public_keys(&format!("http://{addr}")).await;

        assert_eq!(keys, CACHE_NIXOS_ORG_KEY);
    }

    #[tokio::test]
    async fn trusted_public_keys_includes_a_key_the_mirror_publishes() {
        let (server, mock) = mirror_serving(200, &format!("{MIRROR_KEY}\n")).await;

        let keys = trusted_public_keys(&server.url()).await;

        assert_eq!(keys, format!("{CACHE_NIXOS_ORG_KEY} {MIRROR_KEY}"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn trusted_public_keys_reads_the_key_from_under_the_mirror_cache_path() {
        let (server, mock) = mirror_serving(200, MIRROR_KEY).await;

        trusted_public_keys(&format!("{}/", server.url())).await;

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn trusted_public_keys_ignores_a_mirror_that_does_not_publish_a_key() {
        let (server, _mock) = mirror_serving(404, "<html>not found</html>").await;

        assert_eq!(
            trusted_public_keys(&server.url()).await,
            CACHE_NIXOS_ORG_KEY
        );
    }

    #[tokio::test]
    async fn trusted_public_keys_ignores_a_mirror_that_errors() {
        let (server, _mock) = mirror_serving(500, "boom").await;

        assert_eq!(
            trusted_public_keys(&server.url()).await,
            CACHE_NIXOS_ORG_KEY
        );
    }

    #[tokio::test]
    async fn trusted_public_keys_ignores_an_empty_key_file() {
        let (server, _mock) = mirror_serving(200, "").await;

        assert_eq!(
            trusted_public_keys(&server.url()).await,
            CACHE_NIXOS_ORG_KEY
        );
    }

    #[tokio::test]
    async fn trusted_public_keys_ignores_a_blank_key_file() {
        let (server, _mock) = mirror_serving(200, "  \n\t\n").await;

        assert_eq!(
            trusted_public_keys(&server.url()).await,
            CACHE_NIXOS_ORG_KEY
        );
    }

    #[tokio::test]
    async fn trusted_public_keys_keeps_the_default_cache_key_first() {
        let (server, _mock) = mirror_serving(200, &format!("  {MIRROR_KEY}  ")).await;

        let keys = trusted_public_keys(&server.url()).await;

        assert!(keys.starts_with(CACHE_NIXOS_ORG_KEY));
        assert_eq!(keys.split(' ').count(), 2);
    }
}
