//! What the client may depend on directly: arguments, routing, transport and signals. Anything
//! else belongs in the crates it uses.

const ALLOWED: &[&str] = &[
    "clap",
    "mix-events",
    "mix-render",
    "mix-rpc",
    "nix",
    "tokio",
    "uuid",
];

fn dependencies() -> Vec<&'static str> {
    include_str!("../Cargo.toml")
        .split_once("\n[dependencies]\n")
        .expect("the manifest lists dependencies")
        .1
        .lines()
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once(" = ").map(|(name, _)| name.trim()))
        .collect()
}

#[test]
fn the_client_depends_directly_only_on_what_it_is() {
    let extra: Vec<&str> = dependencies()
        .into_iter()
        .filter(|name| !ALLOWED.contains(name))
        .collect();

    assert_eq!(extra, Vec::<&str>::new());
}
