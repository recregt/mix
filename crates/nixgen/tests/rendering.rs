mod support;

use std::path::Path;

use mix_nixgen::{
    CopyIntoGeneration, FileName, FlakeConfig, HomeModule, Rev, StateVersion, System,
};
use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};
use proptest::collection::vec;
use proptest::prelude::*;

const NIXPKGS: Rev = Rev::new_static(NIXPKGS_REV);
const HOME_MANAGER: Rev = Rev::new_static(HOME_MANAGER_REV);
const STATE_VERSION: StateVersion = StateVersion::new_static("24.05");

fn assert_parses(source: &str) {
    let output = support::parse_with_nix(source);
    assert!(
        output.status.success(),
        "nix-instantiate --parse failed:\n{}\n{source}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires nix-instantiate on PATH"]
fn a_realistic_home_module_is_syntactically_valid_nix() {
    let rendered = HomeModule::new("mix", Path::new("/home/mix"), STATE_VERSION)
        .unwrap()
        .copy_into_generation(CopyIntoGeneration {
            source: FileName::new_static("state"),
            target: FileName::new_static("mix-state"),
        })
        .packages(["firefox", "git", "node-sass"])
        .unwrap()
        .render();

    assert_parses(&rendered);
}

#[test]
#[ignore = "requires nix-instantiate on PATH"]
fn a_realistic_flake_is_syntactically_valid_nix() {
    let rendered = FlakeConfig::new(System::X86_64Linux, "mix", NIXPKGS, HOME_MANAGER)
        .unwrap()
        .render();

    assert_parses(&rendered);
}

fn arbitrary_text() -> impl Strategy<Value = String> {
    vec(
        any::<char>().prop_filter("no NUL: unrepresentable in a nix string", |c| *c != '\0'),
        1..20,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn package_name() -> impl Strategy<Value = String> {
    "[a-zA-Z_][a-zA-Z0-9_'-]{0,8}".prop_filter("not a keyword", |s| mix_nixgen::is_identifier(s))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    #[ignore = "requires nix-instantiate on PATH"]
    fn arbitrary_home_modules_always_render_parseable_nix(
        username in arbitrary_text(),
        home in arbitrary_text(),
        packages in vec(package_name(), 0..8),
    ) {
        let home = format!("/{home}");
        if let Ok(module) = HomeModule::new(&username, Path::new(&home), STATE_VERSION) {
            let rendered = module.packages(&packages).unwrap().render();
            let output = support::parse_with_nix(&rendered);
            prop_assert!(
                output.status.success(),
                "parse failed:\n{}\n{rendered}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    #[ignore = "requires nix-instantiate on PATH"]
    fn arbitrary_usernames_always_render_a_parseable_flake(
        username in arbitrary_text()
    ) {
        let rendered = FlakeConfig::new(System::X86_64Linux, &username, NIXPKGS, HOME_MANAGER)
            .unwrap()
            .render();

        let output = support::parse_with_nix(&rendered);
        prop_assert!(
            output.status.success(),
            "parse failed for username {username:?}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
