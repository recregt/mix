use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::process::{Child, Command};

pub const GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct ProcessSet(Arc<Mutex<BTreeSet<i32>>>);

impl ProcessSet {
    fn live(&self) -> std::sync::MutexGuard<'_, BTreeSet<i32>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn signal(&self, signal: Signal) {
        for pgid in self.live().iter() {
            let _ = killpg(Pid::from_raw(*pgid), signal);
        }
    }

    pub fn kill(&self) {
        self.signal(Signal::SIGKILL);
    }

    pub fn pause(&self) {
        self.signal(Signal::SIGTSTP);
    }

    pub fn resume(&self) {
        self.signal(Signal::SIGCONT);
    }

    pub fn same_set(&self, other: &ProcessSet) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

pub struct Group {
    pgid: i32,
    set: ProcessSet,
}

impl Group {
    fn signal(&self, signal: Signal) {
        let _ = killpg(Pid::from_raw(self.pgid), signal);
    }

    pub async fn stop(&self, child: &mut Child, grace: Duration) {
        self.signal(Signal::SIGTERM);
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            self.signal(Signal::SIGKILL);
            let _ = child.wait().await;
        }
        self.signal(Signal::SIGKILL);
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.set.live().remove(&self.pgid);
    }
}

pub(crate) fn spawn(command: &mut Command, set: &ProcessSet) -> std::io::Result<(Child, Group)> {
    let mut live = set.live();
    let child = command.process_group(0).kill_on_drop(true).spawn()?;
    let pgid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or_else(|| std::io::Error::other("the child exited before it could be tracked"))?;
    live.insert(pgid);
    drop(live);
    Ok((
        child,
        Group {
            pgid,
            set: set.clone(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use super::*;

    fn sh(script: &str) -> Command {
        let mut command = crate::Command::new("sh").args(["-c", script]).process();
        command.stdout(Stdio::piped()).stderr(Stdio::null());
        command
    }

    fn exited(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        }
    }

    async fn exits_soon(pid: i32) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if exited(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    async fn read_pid(child: &mut Child) -> i32 {
        use tokio::io::AsyncBufReadExt;
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        tokio::io::BufReader::new(stdout)
            .read_line(&mut line)
            .await
            .unwrap();
        line.trim().parse().unwrap()
    }

    #[tokio::test]
    async fn a_child_leads_its_own_process_group() {
        let (child, group) = spawn(&mut sh("sleep 5"), &ProcessSet::default()).unwrap();

        let pid = child.id().unwrap() as i32;
        assert_eq!(
            nix::unistd::getpgid(Some(Pid::from_raw(pid)))
                .unwrap()
                .as_raw(),
            pid
        );
        assert_ne!(pid, nix::unistd::getpgrp().as_raw());
        drop(group);
    }

    #[tokio::test]
    async fn stop_asks_first_and_the_whole_group_goes() {
        let (mut child, group) =
            spawn(&mut sh("sleep 30 & echo $!; wait"), &ProcessSet::default()).unwrap();
        let grandchild = read_pid(&mut child).await;

        group.stop(&mut child, Duration::from_secs(5)).await;

        assert!(child.try_wait().unwrap().is_some());
        assert!(
            exits_soon(grandchild).await,
            "the grandchild outlived its group"
        );
    }

    #[tokio::test]
    async fn a_child_that_ignores_the_request_is_killed_after_the_grace_period() {
        let (mut child, group) = spawn(
            &mut sh("trap '' TERM; echo $$; while :; do sleep 0.1; done"),
            &ProcessSet::default(),
        )
        .unwrap();
        let _ = read_pid(&mut child).await;

        let started = std::time::Instant::now();
        group.stop(&mut child, Duration::from_millis(300)).await;

        assert!(child.try_wait().unwrap().is_some());
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[tokio::test]
    async fn a_finished_child_is_no_longer_tracked() {
        let set = ProcessSet::default();
        let (mut child, group) = spawn(&mut sh("true"), &set).unwrap();
        let pgid = group.pgid;
        child.wait().await.unwrap();

        drop(group);

        assert!(!set.live().contains(&pgid));
    }
}
