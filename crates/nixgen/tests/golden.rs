mod support;

use mix_nixgen::{FlakeConfig, HomeManagerConfig, Rev, System};
use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

const GOLDEN_FLAKE: &str = include_str!("golden/flake.nix");
const GOLDEN_HOME: &str = include_str!("golden/home.nix");

fn flake() -> String {
    FlakeConfig::new(
        System::X86_64Linux,
        "mix",
        Rev::new_static(NIXPKGS_REV),
        Rev::new_static(HOME_MANAGER_REV),
    )
    .unwrap()
    .render()
}

fn home() -> String {
    let mut cfg = HomeManagerConfig::new();
    cfg.set_str("home.username", "mix")
        .unwrap()
        .set_str("home.homeDirectory", "/home/mix")
        .unwrap()
        .set_str("home.stateVersion", "24.05")
        .unwrap()
        .copy_into_generation("state", "mix-state")
        .unwrap()
        .packages(["git", "ripgrep", "jq", "hello", "tree"])
        .unwrap();
    cfg.render()
}

#[test]
fn the_flake_renders_byte_for_byte() {
    assert_eq!(flake(), GOLDEN_FLAKE);
}

#[test]
fn home_nix_renders_byte_for_byte() {
    assert_eq!(home(), GOLDEN_HOME);
}

#[test]
#[ignore = "requires nix-instantiate on PATH"]
fn the_golden_files_are_nix() {
    for source in [GOLDEN_FLAKE, GOLDEN_HOME] {
        let output = support::parse_with_nix(source);
        assert!(
            output.status.success(),
            "nix-instantiate --parse failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
