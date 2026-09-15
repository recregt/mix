use std::io::Write as _;
use std::path::PathBuf;

use arbitrary::{Arbitrary, Unstructured};
use mix_nixgen_fuzz::{Input, render_with_log, surviving_str_values};

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

        let parse_output = parse_with_nix(&rendered);
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
            let output = eval_raw_attr(&rendered, &attr_path);
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

fn parse_with_nix(src: &str) -> std::process::Output {
    let mut child = std::process::Command::new("nix-instantiate")
        .arg("--parse")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn nix-instantiate");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(src.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn eval_raw_attr(rendered: &str, attr_path: &str) -> std::process::Output {
    let mut file = tempfile::NamedTempFile::new().expect("creating a temp file for nix eval");
    file.write_all(rendered.as_bytes())
        .expect("writing the rendered config to a temp file");

    std::process::Command::new("nix")
        .args(["--extra-experimental-features", "nix-command"])
        .args(["eval", "-f"])
        .arg(file.path())
        .args(["--arg", "pkgs", "{}"])
        .arg("--raw")
        .arg(attr_path)
        .output()
        .expect("failed to run nix eval")
}
