mod support;

use std::path::Path;

use mix_nixgen::{
    CopyIntoGeneration, FileName, FlakeConfig, HomeModule, Rev, StateVersion, System,
};
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
    HomeModule::new(
        "mix",
        Path::new("/home/mix"),
        StateVersion::new_static("24.05"),
    )
    .unwrap()
    .copy_into_generation(CopyIntoGeneration {
        source: FileName::new_static("state"),
        target: FileName::new_static("mix-state"),
    })
    .packages(["git", "ripgrep", "jq", "hello", "tree"])
    .unwrap()
    .render()
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
