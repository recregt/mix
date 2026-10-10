use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::process::{Child, Command};

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

    #[cfg(test)]
    pub(crate) fn same_set(&self, other: &ProcessSet) -> bool {
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

    pub async fn stop(&self, child: &mut Child) {
        self.signal(Signal::SIGTERM);
        self.signal(Signal::SIGCONT);
        let _ = child.wait().await;
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

    use tokio::io::{AsyncBufReadExt, BufReader, Lines};
    use tokio::process::ChildStdout;

    use super::*;

    mix_testchild::install!();

    fn child(steps: &[&str]) -> Command {
        let mut command = crate::Command::new(mix_testchild::program())
            .args(mix_testchild::args(steps.iter().copied()))
            .process();
        command.stdout(Stdio::piped()).stderr(Stdio::null());
        command
    }

    fn said(child: &mut Child) -> Lines<BufReader<ChildStdout>> {
        BufReader::new(child.stdout.take().unwrap()).lines()
    }

    async fn next(lines: &mut Lines<BufReader<ChildStdout>>) -> String {
        lines
            .next_line()
            .await
            .unwrap()
            .expect("the child said more")
    }

    #[tokio::test]
    async fn a_child_leads_its_own_process_group() {
        let (child, group) = spawn(&mut child(&["sleep"]), &ProcessSet::default()).unwrap();

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
            spawn(&mut child(&["spawn", "wait"]), &ProcessSet::default()).unwrap();
        let grandchild: i32 = next(&mut said(&mut child)).await.parse().unwrap();

        group.stop(&mut child).await;

        assert!(child.try_wait().unwrap().is_some());
        tokio::task::spawn_blocking(move || mix_testchild::ended(grandchild))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_stop_reaches_a_frozen_child() {
        let (mut child, group) = spawn(&mut child(&["sleep"]), &ProcessSet::default()).unwrap();
        group.signal(Signal::SIGSTOP);

        group.stop(&mut child).await;

        assert!(child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn a_stop_waits_for_the_child_until_the_request_is_killed() {
        let set = ProcessSet::default();
        let (mut child, group) = spawn(&mut child(&["trap-term", "pid", "sleep"]), &set).unwrap();
        let mut lines = said(&mut child);
        next(&mut lines).await;

        let stopping = tokio::spawn(async move {
            group.stop(&mut child).await;
            child
        });
        assert_eq!(next(&mut lines).await, "term");
        assert!(
            !stopping.is_finished(),
            "the child was asked to stop and is still running, so the stop still waits"
        );

        set.kill();

        let mut child = stopping.await.unwrap();
        assert!(child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn a_finished_child_is_no_longer_tracked() {
        let set = ProcessSet::default();
        let (mut child, group) = spawn(&mut child(&[]), &set).unwrap();
        let pgid = group.pgid;
        child.wait().await.unwrap();

        drop(group);

        assert!(!set.live().contains(&pgid));
    }
}
