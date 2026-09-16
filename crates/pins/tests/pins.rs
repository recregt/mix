use mix_pins::{HOME_MANAGER_REV, NIX_TARBALLS, NIXPKGS_REV, pin_for};

#[test]
fn every_pin_has_a_well_formed_url_and_digest() {
    for pin in NIX_TARBALLS {
        assert!(
            pin.url.starts_with("https://"),
            "{}: url {:?} is not https",
            pin.target,
            pin.url
        );
        assert_eq!(
            pin.sha256.len(),
            64,
            "{}: sha256 {:?} is not 64 hex characters",
            pin.target,
            pin.sha256
        );
        assert!(
            pin.sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{}: sha256 {:?} is not lowercase hex",
            pin.target,
            pin.sha256
        );
    }
}

#[test]
fn the_pinned_flake_inputs_are_full_commit_revisions() {
    for (name, rev) in [("nixpkgs", NIXPKGS_REV), ("home-manager", HOME_MANAGER_REV)] {
        assert_eq!(
            rev.len(),
            40,
            "{name}: revision {rev:?} is not a 40 character commit hash"
        );
        assert!(
            rev.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{name}: revision {rev:?} is not lowercase hex"
        );
    }
}

#[test]
fn pin_for_returns_none_for_an_unknown_target() {
    assert!(pin_for("sparc64-solaris").is_none());
}
