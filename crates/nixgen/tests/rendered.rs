use std::path::Path;

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

fn flake_for(user: &str) -> String {
    FlakeConfig::new(
        System::X86_64Linux,
        user,
        Rev::new_static(NIXPKGS_REV),
        Rev::new_static(HOME_MANAGER_REV),
    )
    .unwrap()
    .render()
}

fn input_block<'a>(flake: &'a str, input: &str) -> &'a str {
    let start = flake.find(&format!("\n    {input} = {{")).unwrap() + 1;
    let mut depth = 0;
    for (offset, character) in flake[start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &flake[start..start + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("the {input} input is never closed");
}

fn packages_listed(home: &str) -> Vec<&str> {
    let (_, rest) = home.split_once("packages = [").unwrap();
    let (list, _) = rest.split_once("];").unwrap();
    list.split_whitespace().collect()
}

#[test]
fn the_flake_pins_both_inputs_to_the_revisions_mix_was_built_with() {
    let flake = flake();

    assert!(flake.contains(&format!("url = \"github:NixOS/nixpkgs/{NIXPKGS_REV}\";")));
    assert!(flake.contains(&format!(
        "url = \"github:nix-community/home-manager/{HOME_MANAGER_REV}\";"
    )));
}

#[test]
fn home_manager_follows_the_pinned_nixpkgs_instead_of_bringing_its_own() {
    let flake = flake();

    assert!(input_block(&flake, "home-manager").contains("follows = \"nixpkgs\";"));
    assert!(!input_block(&flake, "nixpkgs").contains("follows"));
}

#[test]
fn the_flake_exposes_the_configuration_under_the_users_name() {
    for user in ["mix", "alice", "dev"] {
        let flake = flake_for(user);

        assert!(
            flake.contains(&format!("homeConfigurations = {{\n      {user} = ")),
            "{user}: {flake}"
        );
    }
}

#[test]
fn the_home_module_lists_the_packages_in_the_order_it_was_given() {
    assert_eq!(
        packages_listed(&home()),
        [
            "pkgs.git",
            "pkgs.ripgrep",
            "pkgs.jq",
            "pkgs.hello",
            "pkgs.tree"
        ]
    );
}

#[test]
fn the_home_module_names_its_owner_and_copies_the_state_file_into_the_generation() {
    let home = home();

    assert!(home.contains("username = \"mix\";"));
    assert!(home.contains("homeDirectory = \"/home/mix\";"));
    assert!(home.contains("stateVersion = \"24.05\";"));
    assert!(home.contains("cp ${./state} $out/mix-state"));
}

#[test]
fn rendering_the_same_input_twice_gives_the_same_bytes() {
    assert_eq!(flake(), flake());
    assert_eq!(home(), home());
}

#[test]
fn a_rendered_file_parses_and_prints_back_byte_for_byte() {
    for source in [flake(), home()] {
        let parsed = mix_nixgen::parse::parse(&source).unwrap();
        assert_eq!(parsed.print() + "\n", source.split_once("\n\n").unwrap().1);
    }
}
