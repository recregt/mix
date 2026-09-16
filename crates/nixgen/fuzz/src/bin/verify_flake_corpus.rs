use std::path::PathBuf;

use arbitrary::{Arbitrary, Unstructured};
use mix_nixgen_fuzz::{FlakeInput, nix, render_flake, rendered_username_key};

fn main() {
    let corpus_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("corpus/flake"));

    let mut checked = 0usize;
    let mut parse_failed = 0usize;
    let mut key_failed = 0usize;

    for entry in std::fs::read_dir(&corpus_dir).expect("reading the corpus directory") {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };

        let u = Unstructured::new(&bytes);
        let Ok(input) = FlakeInput::arbitrary_take_rest(u) else {
            continue;
        };
        let Some(rendered) = render_flake(&input) else {
            continue;
        };
        checked += 1;

        let parse_output = nix::parse(&rendered);
        if !parse_output.status.success() {
            parse_failed += 1;
            eprintln!("=== FAILED TO PARSE: {} ===", path.display());
            eprintln!("{rendered}");
            eprintln!("--- nix-instantiate stderr ---");
            eprintln!("{}", String::from_utf8_lossy(&parse_output.stderr));
            continue;
        }

        let Some(key) = rendered_username_key(&rendered) else {
            key_failed += 1;
            eprintln!("=== NO HOME CONFIGURATION KEY: {} ===", path.display());
            eprintln!("{rendered}");
            continue;
        };

        let probe = format!("{{ key = builtins.head (builtins.attrNames {{ {key} = 1; }}); }}");
        let output = nix::eval_raw_attr(&probe, "key");
        if !output.status.success() {
            key_failed += 1;
            eprintln!("=== FAILED TO EVALUATE THE KEY: {} ===", path.display());
            eprintln!("{probe}");
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            continue;
        }
        let actual = String::from_utf8_lossy(&output.stdout).into_owned();
        if actual != input.username {
            key_failed += 1;
            eprintln!("=== KEY ROUND-TRIP MISMATCH: {} ===", path.display());
            eprintln!("expected: {:?}", input.username);
            eprintln!("actual:   {actual:?}");
        }
    }

    println!(
        "checked {checked} corpus entries: {parse_failed} failed to parse, \
         {key_failed} usernames failed to round-trip as an attribute key"
    );
    if parse_failed > 0 || key_failed > 0 {
        std::process::exit(1);
    }
}
