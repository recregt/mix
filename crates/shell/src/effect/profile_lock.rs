//! Nix's locks on a user's profiles, waited for where the reader can see it.
//!
//! Note: Nix holds `<profile>.lock` with `flock` while it changes a profile, so a `nix-env` or
//! `nix profile` the user runs makes mix's own profile commands wait.

use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_core::ActivityReporter;
use mix_core::action::Failure;
use mix_core::identity::InvokingUser;
use mix_core::paths::{
    HOME_MANAGER_PROFILE_NAME, PROFILE_LOCK_SUFFIX, USER_PROFILE_NAME, nix_profiles_dir,
};
use mix_exec::Scope;
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg, OFlag};
use nix::sys::stat::Mode;

/// Kernel table of the file locks held on the machine.
const PROC_LOCKS: &str = "/proc/locks";

/// The locks Nix takes on the profiles mix changes for `user`.
fn locks(user: &InvokingUser) -> [PathBuf; 2] {
    let profiles = nix_profiles_dir(&user.home);
    [HOME_MANAGER_PROFILE_NAME, USER_PROFILE_NAME]
        .map(|profile| profiles.join(format!("{profile}{PROFILE_LOCK_SUFFIX}")))
}

/// Returns once no other process holds a lock Nix takes on `user`'s profiles, reporting the
/// holder of each lock it waits for.
///
/// Note: A lock is only waited for, never kept: Nix takes it again for the command that follows.
pub async fn wait(
    user: &InvokingUser,
    activity: &Arc<dyn ActivityReporter>,
    scope: &Scope,
) -> Result<(), Failure> {
    for lock in locks(user) {
        let Some(file) = open(&lock) else {
            continue;
        };
        let file = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(_free) => continue,
            Err((file, Errno::EWOULDBLOCK)) => file,
            Err(_) => continue,
        };
        let holder = holder(&lock);
        activity.waiting(
            &lock.display().to_string(),
            holder.as_ref().map(|(user, _)| user.as_str()),
            holder.as_ref().map(|(_, command)| command.as_str()),
        );
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            if let Ok(held) = Flock::lock(file, FlockArg::LockExclusive) {
                let _ = sender.send(held);
            }
        });
        if scope.guard(receiver).await.is_err() {
            return Err(Failure::Cancelled);
        }
    }
    Ok(())
}

fn open(lock: &Path) -> Option<File> {
    nix::fcntl::open(
        lock,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .ok()
    .map(File::from)
}

/// The user and command line of the process holding `lock`, read from `/proc/locks`.
fn holder(lock: &Path) -> Option<(String, String)> {
    let inode = std::fs::symlink_metadata(lock).ok()?.ino();
    let table = std::fs::read_to_string(PROC_LOCKS).ok()?;
    let pid = holding(&table, inode)?;
    let process = PathBuf::from(format!("/proc/{pid}"));
    let uid = std::fs::metadata(&process).ok()?.uid();
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map_or_else(|| format!("uid {uid}"), |user| user.name);
    let command = std::fs::read(process.join("cmdline"))
        .ok()?
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(" ");
    Some((user, command))
}

/// The pid holding a `flock` on `inode` in a `/proc/locks` table.
fn holding(table: &str, inode: u64) -> Option<u32> {
    table.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(1) != Some(&"FLOCK") {
            return None;
        }
        let found: u64 = fields.get(5)?.rsplit(':').next()?.parse().ok()?;
        (found == inode)
            .then(|| fields.get(4)?.parse().ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "1: POSIX  ADVISORY  WRITE 812 00:2a:4711 0 EOF\n\
                         2: FLOCK  ADVISORY  WRITE 1903 00:2a:12345 0 EOF\n\
                         2: -> FLOCK  ADVISORY  WRITE 2001 00:2a:12345 0 EOF\n";

    #[test]
    fn the_holder_is_the_process_with_the_granted_flock_on_the_inode() {
        assert_eq!(holding(TABLE, 12345), Some(1903));
        assert_eq!(holding(TABLE, 4711), None);
        assert_eq!(holding(TABLE, 1), None);
    }

    #[test]
    fn the_locks_are_the_ones_nix_takes_on_both_profiles() {
        let user = InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".into(),
            home: "/home/alice".into(),
        };

        assert_eq!(
            locks(&user),
            [
                PathBuf::from("/home/alice/.local/state/nix/profiles/home-manager.lock"),
                PathBuf::from("/home/alice/.local/state/nix/profiles/profile.lock"),
            ]
        );
    }

    #[derive(Default)]
    struct Recorded(std::sync::Mutex<Vec<String>>);

    impl ActivityReporter for Recorded {
        fn line(&self, _line: &str) {}
        fn progress(&self, _progress: &mix_core::BuildProgress) {}
        fn clear(&self) {}
        fn waiting(&self, lock: &str, _holder: Option<&str>, _command: Option<&str>) {
            self.0.lock().unwrap().push(lock.to_string());
        }
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_held_lock_is_reported_and_waited_for_until_it_is_released() {
        let home = tempfile::tempdir().unwrap();
        let user = InvokingUser {
            uid: nix::unistd::Uid::current().as_raw(),
            gid: nix::unistd::Gid::current().as_raw(),
            name: "mix-user".into(),
            home: home.path().to_path_buf(),
        };
        let [home_manager, profile] = locks(&user);
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&home_manager, "").unwrap();
        std::fs::write(&profile, "").unwrap();
        let held = Flock::lock(File::open(&profile).unwrap(), FlockArg::LockExclusive).unwrap();
        let recorded = Arc::new(Recorded::default());
        let activity: Arc<dyn ActivityReporter> = recorded.clone();
        let scope = Scope::root();

        let waiting = tokio::spawn(async move { wait(&user, &activity, &scope).await });
        while recorded.0.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        assert!(!waiting.is_finished());
        drop(held);

        assert!(waiting.await.unwrap().is_ok());
        assert_eq!(*recorded.0.lock().unwrap(), [profile.display().to_string()]);
    }
}
