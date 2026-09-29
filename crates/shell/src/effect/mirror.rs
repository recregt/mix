//! Where nix is fetched from: the archive, the flake inputs, and the binary cache.

use mix_pins::{HOME_MANAGER_NAR_HASH, HOME_MANAGER_REV, NIXPKGS_NAR_HASH, NIXPKGS_REV};

pub fn filter_mirror(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

pub fn mirror_url(base: &str, filename: &str) -> String {
    format!("{}/{filename}", base.trim().trim_end_matches('/'))
}

pub fn nixpkgs_override(base: &str) -> String {
    format!(
        "tarball+{}?narHash={}",
        mirror_url(base, &format!("nixpkgs-{NIXPKGS_REV}.tar.gz")),
        query_value(NIXPKGS_NAR_HASH)
    )
}

pub fn home_manager_override(base: &str) -> String {
    format!(
        "tarball+{}?narHash={}",
        mirror_url(base, &format!("home-manager-{HOME_MANAGER_REV}.tar.gz")),
        query_value(HOME_MANAGER_NAR_HASH)
    )
}

fn query_value(sri: &str) -> String {
    sri.replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_nar_hash_is_percent_encoded_for_a_query() {
        assert_eq!(query_value("sha256-a+b/c="), "sha256-a%2Bb%2Fc%3D");
    }

    #[test]
    fn nixpkgs_override_uses_a_tarball_scheme_keyed_by_the_pinned_revision() {
        assert_eq!(
            nixpkgs_override("http://mirror.internal"),
            format!(
                "tarball+http://mirror.internal/nixpkgs-{NIXPKGS_REV}.tar.gz?narHash={}",
                query_value(NIXPKGS_NAR_HASH)
            )
        );
    }

    #[test]
    fn home_manager_override_uses_a_tarball_scheme_keyed_by_the_pinned_revision() {
        assert_eq!(
            home_manager_override("http://mirror.internal"),
            format!(
                "tarball+http://mirror.internal/home-manager-{HOME_MANAGER_REV}.tar.gz?narHash={}",
                query_value(HOME_MANAGER_NAR_HASH)
            )
        );
    }
}
