use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use mix_core::lock::acquire_exclusive;

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn a_new_lock_is_readable_by_everyone_whatever_the_umask() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix").join("lock");

    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let guard = acquire_exclusive(&path);
    nix::sys::stat::umask(previous);
    let _guard = guard.unwrap();

    assert_eq!(mode(&path), 0o644);
    assert_eq!(mode(path.parent().unwrap()), 0o755);
}
