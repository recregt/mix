#![allow(clippy::disallowed_macros)]
#![allow(dead_code)]

#[path = "src/ast.rs"]
mod ast;
#[path = "src/escape.rs"]
mod escape;
#[path = "src/flake_write.rs"]
mod flake_write;
#[path = "src/header.rs"]
mod header;
#[path = "src/ident.rs"]
mod ident;
#[path = "src/print.rs"]
mod print;

use std::fmt::Write as _;

const USERNAME: &str = "__mix_hole_username__";
const SYSTEM: &str = "__mix_hole_system__";
const NIXPKGS_REV: &str = "__mix_hole_nixpkgs_rev__";
const HOME_MANAGER_REV: &str = "__mix_hole_home_manager_rev__";

const HOLES: [(&str, &str); 4] = [
    (USERNAME, "Username"),
    (SYSTEM, "System"),
    (NIXPKGS_REV, "NixpkgsRev"),
    (HOME_MANAGER_REV, "HomeManagerRev"),
];

const SOURCES: [&str; 7] = [
    "build.rs",
    "src/ast.rs",
    "src/escape.rs",
    "src/flake_write.rs",
    "src/header.rs",
    "src/ident.rs",
    "src/print.rs",
];

fn main() {
    for source in SOURCES {
        println!("cargo::rerun-if-changed={source}");
    }

    let username = ast::Key::new_static(USERNAME);
    let system = ast::Key::new_static(SYSTEM);
    let mut text = String::from(header::GENERATED_HEADER);
    flake_write::write_flake(
        &mut print::Writer::new(&mut text),
        &username,
        &system,
        ast::Verbatim::new_static(NIXPKGS_REV),
        ast::Verbatim::new_static(HOME_MANAGER_REV),
    );
    text.push('\n');

    let mut chunks = Vec::new();
    let mut holes = Vec::new();
    let mut rest = text.as_str();
    while let Some((at, marker, name)) = HOLES
        .iter()
        .filter_map(|(marker, name)| rest.find(marker).map(|at| (at, *marker, *name)))
        .min_by_key(|(at, _, _)| *at)
    {
        chunks.push(&rest[..at]);
        holes.push(name);
        rest = &rest[at + marker.len()..];
    }
    chunks.push(rest);
    for (_, name) in HOLES {
        assert_eq!(
            holes.iter().filter(|hole| **hole == name).count(),
            1,
            "the flake must hold the {name} hole exactly once"
        );
    }

    let mut generated = String::new();
    writeln!(
        generated,
        "const FLAKE_CHUNKS: [&str; {}] = [",
        chunks.len()
    )
    .unwrap();
    for chunk in &chunks {
        writeln!(generated, "    {chunk:?},").unwrap();
    }
    writeln!(generated, "];").unwrap();
    writeln!(
        generated,
        "const FLAKE_HOLES: [FlakeHole; {}] = [",
        holes.len()
    )
    .unwrap();
    for hole in &holes {
        writeln!(generated, "    FlakeHole::{hole},").unwrap();
    }
    writeln!(generated, "];").unwrap();
    let length: usize = chunks.iter().map(|chunk| chunk.len()).sum();
    writeln!(generated, "const FLAKE_TEMPLATE_LEN: usize = {length};").unwrap();

    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts");
    std::fs::write(
        std::path::Path::new(&out_dir).join("flake_template.rs"),
        generated,
    )
    .expect("writing the generated flake template");
}
