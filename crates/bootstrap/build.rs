use std::fmt::Write as _;

include!("src/pins.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=MIX_NIX_TARBALL_PATH");
    println!("cargo:rerun-if-env-changed=MIX_NIX_TARGET");
    println!("cargo:rerun-if-changed=src/pins.rs");

    if std::env::var_os("CARGO_FEATURE_EMBED_TARBALL").is_none() {
        return;
    }

    let rustc_target = std::env::var("MIX_NIX_TARGET")
        .or_else(|_| std::env::var("TARGET"))
        .expect("cargo always sets TARGET");
    let key = normalize_target(&rustc_target);

    let pin = pin_for(&key).unwrap_or_else(|| {
        panic!(
            "no pinned Nix tarball for target `{key}` (from rustc target `{rustc_target}`); \
             add one to crates/bootstrap/src/pins.rs"
        )
    });

    let path = std::env::var("MIX_NIX_TARBALL_PATH").unwrap_or_else(|_| {
        panic!(
            "building with --features embed-tarball requires MIX_NIX_TARBALL_PATH to point \
             at a downloaded copy of {} -- run scripts/bump-nix.sh or download it yourself",
            pin.url
        )
    });

    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("reading MIX_NIX_TARBALL_PATH ({path}): {e}"));

    let digest = sha256_hex(&bytes);
    assert_eq!(
        digest, pin.sha256,
        "downloaded tarball at {path} does not match the pin for {key} in src/pins.rs \
         (expected {}, got {digest})",
        pin.sha256,
    );

    let absolute =
        std::fs::canonicalize(&path).unwrap_or_else(|e| panic!("canonicalizing {path}: {e}"));
    println!(
        "cargo:rustc-env=MIX_NIX_TARBALL_PATH={}",
        absolute.display()
    );
}

fn normalize_target(rustc_target: &str) -> String {
    let arch = rustc_target.split('-').next().unwrap_or(rustc_target);
    if rustc_target.contains("linux") {
        format!("{arch}-linux")
    } else if rustc_target.contains("darwin") {
        format!("{arch}-darwin")
    } else {
        rustc_target.to_string()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(out, "{byte:02x}").expect("writing to a String never fails");
    }
    out
}
