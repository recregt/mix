use std::path::PathBuf;

use arbitrary::{Arbitrary, Unstructured};
use mix_nixgen_fuzz::{Input, nix, render_with_log, surviving_str_values};

fn main() {
    let corpus_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("corpus/codegen"));

    let mut checked = 0usize;
    let mut parse_failed = 0usize;
    let mut round_trips_checked = 0usize;
    let mut round_trip_failed = 0usize;

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
        let Ok(input) = Input::arbitrary_take_rest(u) else {
            continue;
        };

        let (rendered, log) = render_with_log(input);
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

        for (attr_path, expected) in surviving_str_values(&log) {
            round_trips_checked += 1;
            let output = nix::eval_raw_module_attr(&rendered, &attr_path);
            if !output.status.success() {
                round_trip_failed += 1;
                eprintln!("=== FAILED TO EVALUATE {attr_path}: {} ===", path.display());
                eprintln!("{}", String::from_utf8_lossy(&output.stderr));
                continue;
            }
            let actual = String::from_utf8_lossy(&output.stdout).into_owned();
            if actual != expected {
                round_trip_failed += 1;
                eprintln!(
                    "=== ROUND-TRIP MISMATCH {attr_path}: {} ===",
                    path.display()
                );
                eprintln!("expected: {expected:?}");
                eprintln!("actual:   {actual:?}");
            }
        }
    }

    println!(
        "checked {checked} corpus entries: {parse_failed} failed to parse, \
         {round_trips_checked} string values checked, {round_trip_failed} failed to round-trip"
    );
    if parse_failed > 0 || round_trip_failed > 0 {
        std::process::exit(1);
    }
}
