use std::io::Write as _;
use std::process::Output;

pub fn parse(src: &str) -> Output {
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

pub fn eval_raw_module_attr(src: &str, attr_path: &str) -> Output {
    eval_raw(src, &["--arg", "pkgs", "{}"], attr_path)
}

pub fn eval_raw_attr(src: &str, attr_path: &str) -> Output {
    eval_raw(src, &[], attr_path)
}

fn eval_raw(src: &str, args: &[&str], attr_path: &str) -> Output {
    let mut file = tempfile::NamedTempFile::new().expect("creating a temp file for nix eval");
    file.write_all(src.as_bytes())
        .expect("writing the nix expression to a temp file");

    std::process::Command::new("nix")
        .args(["--extra-experimental-features", "nix-command"])
        .args(["eval", "-f"])
        .arg(file.path())
        .args(args)
        .arg("--raw")
        .arg(attr_path)
        .output()
        .expect("failed to run nix eval")
}
