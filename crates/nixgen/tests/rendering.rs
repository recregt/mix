mod support;

use mix_nixgen::{FlakeConfig, HomeManagerConfig};
use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};
use proptest::collection::vec;
use proptest::prelude::*;

#[test]
#[ignore = "requires nix-instantiate on PATH"]
fn a_realistic_config_is_syntactically_valid_nix() {
    let mut cfg = HomeManagerConfig::new();
    cfg.packages(["firefox", "git", "node-sass"]).unwrap();
    cfg.set_bool("programs.git.enable", true).unwrap();
    cfg.set_str("programs.git.userName", "mix").unwrap();
    cfg.set_str("programs.git.userEmail", "user@example.com")
        .unwrap();

    let output = support::parse_with_nix(&cfg.render());
    assert!(
        output.status.success(),
        "nix-instantiate --parse failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires nix-instantiate on PATH"]
fn a_realistic_flake_is_syntactically_valid_nix() {
    let rendered = FlakeConfig::new("x86_64-linux", "mix", NIXPKGS_REV, HOME_MANAGER_REV)
        .unwrap()
        .render();

    let output = support::parse_with_nix(&rendered);
    assert!(
        output.status.success(),
        "nix-instantiate --parse failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dotted_path() -> impl Strategy<Value = String> {
    vec("[a-zA-Z_][a-zA-Z0-9_'-]{0,8}", 1..4).prop_map(|segments| segments.join("."))
}

fn arbitrary_text() -> impl Strategy<Value = String> {
    vec(
        any::<char>().prop_filter("no NUL: unrepresentable in a process argv", |c| *c != '\0'),
        0..20,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    #[ignore = "requires nix-instantiate on PATH"]
    fn arbitrary_valid_paths_always_render_parseable_nix(
        ops in vec((dotted_path(), any::<bool>(), arbitrary_text()), 1..8)
    ) {
        let mut cfg = HomeManagerConfig::new();
        for (path, as_bool, text) in &ops {
            if *as_bool {
                cfg.set_bool(path, true).unwrap();
            } else {
                cfg.set_str(path, text).unwrap();
            }
        }

        let output = support::parse_with_nix(&cfg.render());
        prop_assert!(
            output.status.success(),
            "parse failed for {ops:?}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "requires nix-instantiate on PATH"]
    fn arbitrary_usernames_always_render_a_parseable_flake(
        username in arbitrary_text()
    ) {
        let rendered = FlakeConfig::new("x86_64-linux", &username, NIXPKGS_REV, HOME_MANAGER_REV)
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
