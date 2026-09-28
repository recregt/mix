use mix_pins::{
    HOME_MANAGER_LAST_MODIFIED, HOME_MANAGER_NAR_HASH, HOME_MANAGER_REV, NIXPKGS_LAST_MODIFIED,
    NIXPKGS_NAR_HASH, NIXPKGS_REV,
};

fn locked(flake: &str) -> (String, u64) {
    let command = mix_exec::Command::new("nix")
        .args(["--extra-experimental-features", "nix-command flakes"])
        .args(["flake", "prefetch", "--json"])
        .arg(flake);
    let line = command.line();
    let output = command
        .output_blocking(&mix_exec::Scope::root())
        .unwrap_or_else(|error| panic!("failed to run {line}: {error}"));
    assert!(
        output.status.success(),
        "{line} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let prefetched: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    (
        prefetched["hash"].as_str().unwrap().to_string(),
        prefetched["locked"]["lastModified"].as_u64().unwrap(),
    )
}

#[test]
#[ignore = "requires nix and network access"]
fn the_nixpkgs_pin_matches_upstream() {
    let (nar_hash, last_modified) = locked(&format!("github:NixOS/nixpkgs/{NIXPKGS_REV}"));
    assert_eq!(nar_hash, NIXPKGS_NAR_HASH);
    assert_eq!(last_modified, NIXPKGS_LAST_MODIFIED);
}

#[test]
#[ignore = "requires nix and network access"]
fn the_home_manager_pin_matches_upstream() {
    let (nar_hash, last_modified) = locked(&format!(
        "github:nix-community/home-manager/{HOME_MANAGER_REV}"
    ));
    assert_eq!(nar_hash, HOME_MANAGER_NAR_HASH);
    assert_eq!(last_modified, HOME_MANAGER_LAST_MODIFIED);
}
