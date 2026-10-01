#![allow(clippy::disallowed_methods)]

mod support;

use mix_nixgen::{AttrPath, FlakeRef, Installable};

const FLAKE: &str = "{ outputs = { self }: { x = \"ok\"; }; }\n";

fn flake_in_an_awkward_directory() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("we ird#dir?x%41^");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("flake.nix"), FLAKE).unwrap();
    support::git(
        &dir,
        &["init", "-q", "--initial-branch=main", "--template="],
    );
    support::git(&dir, &["add", "flake.nix"]);
    support::git(&dir, &["commit", "-q", "-m", "flake"]);
    (root, dir)
}

fn eval(installable: &Installable) -> String {
    let output = support::eval_installable(&installable.render());
    assert!(
        output.status.success(),
        "nix eval {} failed:\n{}",
        installable.render(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
#[ignore = "requires nix and git on PATH"]
fn a_path_flake_in_an_awkward_directory_is_found() {
    let (_root, dir) = flake_in_an_awkward_directory();
    let installable =
        Installable::new(FlakeRef::path(&dir).unwrap(), AttrPath::new(["x"]).unwrap());
    assert_eq!(eval(&installable), "ok");
}

#[test]
#[ignore = "requires nix and git on PATH"]
fn a_git_flake_in_an_awkward_directory_is_found() {
    let (_root, dir) = flake_in_an_awkward_directory();
    let installable = Installable::new(
        FlakeRef::git_file(&dir, None).unwrap(),
        AttrPath::new(["x"]).unwrap(),
    );
    assert_eq!(eval(&installable), "ok");
}
