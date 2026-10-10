use std::path::Path;

use insta::assert_snapshot;

use mix_nixgen::{
    CopyIntoGeneration, FileName, FlakeConfig, HomeModule, Rev, StateVersion, System,
};
use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

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
fn the_flake_renders_as_recorded() {
    assert_snapshot!("flake.nix", flake());
}

#[test]
fn home_nix_renders_as_recorded() {
    assert_snapshot!("home.nix", home());
}

#[test]
fn a_rendered_file_parses_and_prints_back_byte_for_byte() {
    for source in [flake(), home()] {
        let parsed = mix_nixgen::parse::parse(&source).unwrap();
        assert_eq!(parsed.print() + "\n", source.split_once("\n\n").unwrap().1);
    }
}
