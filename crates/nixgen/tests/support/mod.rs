#![allow(dead_code)]

use std::io::Write as _;

fn finished(command: mix_exec::Command) -> std::process::Output {
    let line = command.line();
    command
        .output_blocking(&mix_exec::cancel::root())
        .unwrap_or_else(|error| panic!("failed to run {line}: {error}"))
}

pub fn parse_with_nix(src: &str) -> std::process::Output {
    finished(
        mix_exec::Command::new("nix-instantiate")
            .args(["--parse", "-"])
            .input(src.as_bytes().to_vec()),
    )
}

pub fn eval_raw_attr(rendered: &str, attr_path: &str) -> std::process::Output {
    let mut file = tempfile::NamedTempFile::new().expect("creating a temp file for nix eval");
    file.write_all(rendered.as_bytes())
        .expect("writing the rendered config to a temp file");

    finished(
        mix_exec::Command::new("nix")
            .args(["--extra-experimental-features", "nix-command"])
            .args(["eval", "-f"])
            .arg(file.path())
            .args(["--arg", "pkgs", "{}"])
            .arg("--raw")
            .arg(attr_path),
    )
}

pub fn eval_installable(installable: &str) -> std::process::Output {
    finished(
        mix_exec::Command::new("nix")
            .args(["--extra-experimental-features", "nix-command flakes"])
            .args(["eval", "--raw"])
            .arg(installable),
    )
}

pub fn git(dir: &std::path::Path, args: &[&str]) {
    let output = finished(
        mix_exec::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "mix")
            .env("GIT_AUTHOR_EMAIL", "mix@localhost")
            .env("GIT_COMMITTER_NAME", "mix")
            .env("GIT_COMMITTER_EMAIL", "mix@localhost"),
    );
    assert!(output.status.success(), "git {args:?} failed");
}
