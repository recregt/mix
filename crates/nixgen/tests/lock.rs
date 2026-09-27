use mix_nixgen::Rev;
use mix_nixgen::lock::{LockedInput, NarHash, render};
use mix_pins::{
    HOME_MANAGER_LAST_MODIFIED, HOME_MANAGER_NAR_HASH, HOME_MANAGER_REV, NIXPKGS_LAST_MODIFIED,
    NIXPKGS_NAR_HASH, NIXPKGS_REV,
};

const NIX_WROTE: &str = include_str!("fixtures/flake.lock");

#[test]
fn the_lock_is_exactly_what_nix_writes_for_the_pins() {
    let rendered = render(
        LockedInput {
            rev: Rev::new_static(NIXPKGS_REV),
            nar_hash: NarHash::new_static(NIXPKGS_NAR_HASH),
            last_modified: NIXPKGS_LAST_MODIFIED,
        },
        LockedInput {
            rev: Rev::new_static(HOME_MANAGER_REV),
            nar_hash: NarHash::new_static(HOME_MANAGER_NAR_HASH),
            last_modified: HOME_MANAGER_LAST_MODIFIED,
        },
    );
    assert_eq!(rendered, NIX_WROTE);
}
