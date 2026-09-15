#![allow(dead_code)]

use std::io::Write as _;

pub fn parse_with_nix(src: &str) -> std::process::Output {
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

pub fn eval_raw_attr(rendered: &str, attr_path: &str) -> std::process::Output {
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
