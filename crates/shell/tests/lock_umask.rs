use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use mix_events::v1::Command;
use mix_events::{Outbox, Start, Tree};
use mix_shell::lock::{Holder, Locks, Need};

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[tokio::test]
async fn a_new_lock_is_readable_by_everyone_whatever_the_umask() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix").join("lock");
    let locks = Locks::new(&path);
    let mut tree = Tree::new(
        Arc::new(Outbox::new(String::from("r"), || {})),
        Arc::new(|| None),
        Start::command("repair", Command::default()),
    );
    let holder = Holder {
        user: "root".into(),
        command: "repair".into(),
    };

    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let held = locks
        .acquire(
            holder,
            Need::Exclusive,
            None,
            &mut tree,
            &mix_exec::Scope::root(),
            &(Arc::new(|| None) as mix_events::Stopped),
        )
        .await;
    nix::sys::stat::umask(previous);
    let _held = held.unwrap();

    assert_eq!(mode(&path), 0o644);
    assert_eq!(mode(path.parent().unwrap()), 0o755);
}
