use std::path::Path;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use mix_core::action::{Action, Fact, Failure, Outcome, Query};
use mix_core::journal::Record;
use mix_core::paths::SYSTEMD_UNIT_DIR as UNIT_DIR;
use mix_core::plan::{Input, Next, Report, Runner};
use mix_core::{DownloadProgress, Scope};
use mix_events::v1::Bytes;
use mix_events::v1::node_progress::Progress;
use mix_events::{Stopped, Tree};

use crate::effect::files::{Files, Prepared};
use crate::effect::identity;
use crate::effect::runtime;
use crate::effect::units::Units;

struct Relay<'a> {
    report: Mutex<&'a mut (dyn FnMut(Progress) + Send)>,
    total: AtomicU64,
    done: AtomicU64,
}

impl DownloadProgress for Relay<'_> {
    fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    fn add(&self, delta: u64) {
        let done = self.done.fetch_add(delta, Ordering::Relaxed) + delta;
        let total = self.total.load(Ordering::Relaxed);
        if let Ok(mut report) = self.report.lock() {
            report(Progress::Bytes(Bytes {
                done,
                total: (total > 0).then_some(total),
            }));
        }
    }
}

pub struct Performer {
    files: Files,
    units: Option<Units>,
}

impl Performer {
    pub fn new(files: Files) -> Self {
        Self { files, units: None }
    }

    async fn units(&mut self) -> Result<&Units, Failure> {
        if self.units.is_none() {
            self.units = Some(Units::connect().await?);
        }
        Ok(self.units.as_ref().expect("connected above"))
    }

    pub async fn observe(&mut self, queries: &[Query]) -> Result<Vec<Fact>, Failure> {
        let mut facts = Vec::with_capacity(queries.len());
        for query in queries {
            let fact = match query {
                Query::Unit(unit) => Fact::Unit(self.units().await?.observe(unit).await?),
                query => self
                    .files
                    .observe(query)
                    .or_else(|| identity::observe(query))
                    .expect("every query has an observer"),
            };
            facts.push(fact);
        }
        Ok(facts)
    }

    pub async fn perform(
        &mut self,
        action: &Action,
        scope: &Scope,
        progress: &mut (dyn FnMut(Progress) + Send),
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        match action {
            Action::InstallRuntime { url, sha256, size } => {
                let relay = Relay {
                    report: Mutex::new(progress),
                    total: AtomicU64::new(0),
                    done: AtomicU64::new(0),
                };
                return runtime::install(url, sha256, *size, &relay, scope, prepared).await;
            }
            Action::RemoveRuntime { created } => return runtime::remove(created).await,
            Action::InstallUnit {
                unit,
                contents,
                expect,
            } => {
                let mut with_reload = |undo: &[Action]| {
                    let mut undo = undo.to_vec();
                    undo.push(Action::DaemonReload);
                    prepared(&undo)
                };
                let mut performed = self
                    .files
                    .perform(
                        &Action::PutFile {
                            path: Path::new(UNIT_DIR).join(unit),
                            contents: contents.clone(),
                            mode: 0o644,
                            owner: None,
                            expect: *expect,
                        },
                        &mut with_reload,
                    )
                    .expect("a file write")?;
                performed.undo.push(Action::DaemonReload);
                return Ok(performed);
            }
            _ => {}
        }
        if let Some(outcome) = self.files.perform(action, prepared) {
            return outcome;
        }
        if let Some(outcome) = identity::perform(action, scope, prepared).await {
            return outcome;
        }
        if let Some(outcome) = self.units().await?.perform(action, scope, prepared).await {
            return outcome;
        }
        Err(Failure::CommandFailed {
            program: "mix".to_string(),
            status: None,
            output_tail: format!("no performer for {action:?}"),
        })
    }

    pub fn adopt(&mut self, pending: Vec<std::path::PathBuf>) {
        self.files.adopt(pending);
    }
}

pub trait Journal: Send {
    fn append(&mut self, record: &Record) -> Result<(), Failure>;
}

impl Journal for Vec<Record> {
    fn append(&mut self, record: &Record) -> Result<(), Failure> {
        self.push(record.clone());
        Ok(())
    }
}

fn keep(journal: &mut dyn Journal, record: &Record) {
    if let Err(failure) = journal.append(record) {
        tracing::warn!("could not record {record:?} in the journal: {failure:?}");
    }
}

pub async fn drive(
    runner: &mut Runner,
    tree: &mut Tree,
    performer: &mut Performer,
    scope: &Scope,
    stopped: &Stopped,
    journal: &mut dyn Journal,
) -> Report {
    let mut input = None;
    let mut seq = 0;
    let mut ended = false;
    loop {
        if let Some(cause) = stopped() {
            runner.stop(cause);
        }
        match runner.step(tree, input.take()) {
            Next::Observe(queries) => {
                input = Some(Input::Facts(performer.observe(&queries).await));
            }
            Next::Perform(action) => {
                let scope = if runner.shielded() {
                    scope.shielded()
                } else {
                    scope.clone()
                };
                let node = runner.current_node();
                let reverting = runner.rolling_back();
                let committing = action == Action::Commit;
                if committing && let Err(failure) = journal.append(&Record::Committing) {
                    input = Some(Input::Done(Err(failure)));
                    continue;
                }
                let this = seq;
                let mut progress = |progress: Progress| {
                    if let Some(node) = node {
                        let _ = tree.progress(node, progress);
                    }
                };
                let mut prepared = |undo: &[Action]| {
                    if reverting || committing {
                        return Ok(());
                    }
                    journal.append(&Record::Prepared {
                        seq: this,
                        undo: undo.to_vec(),
                    })
                };
                let outcome = performer
                    .perform(&action, &scope, &mut progress, &mut prepared)
                    .await;
                match (&outcome, reverting, committing) {
                    (Ok(_), true, _) => keep(journal, &Record::Reverted { action }),
                    (Ok(_), false, true) => {
                        keep(journal, &Record::Ended);
                        ended = true;
                    }
                    (Ok(_), false, false) => {
                        keep(journal, &Record::Done { seq: this });
                        seq += 1;
                    }
                    (Err(_), false, false) => {
                        keep(journal, &Record::Failed { seq: this });
                        seq += 1;
                    }
                    (Err(_), _, _) => {}
                }
                input = Some(Input::Done(outcome));
            }
            Next::Finished(report) => {
                if !ended {
                    keep(journal, &Record::Ended);
                }
                return report;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use mix_core::action::{Expect, Kind};
    use mix_core::plan::{StepSpec, Verdict};
    use mix_events::v1::{Cancellation, Command};
    use mix_events::{Outbox, ROOT, Start, validate};

    use super::*;

    struct Ensure {
        key: &'static str,
        path: PathBuf,
        contents: Option<&'static str>,
    }

    impl StepSpec for Ensure {
        fn key(&self) -> &'static str {
            self.key
        }

        fn title(&self) -> &'static str {
            "ensure"
        }

        fn queries(&self) -> Vec<Query> {
            vec![Query::Path(self.path.clone())]
        }

        fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
            Ok((|| -> Vec<Action> {
                let Fact::Path(facts) = &facts[0] else {
                    unreachable!()
                };
                if facts.kind != Kind::Missing {
                    return Vec::new();
                }
                vec![match self.contents {
                    None => Action::CreateDir {
                        path: self.path.clone(),
                        mode: 0o755,
                        owner: None,
                    },
                    Some(contents) => Action::PutFile {
                        path: self.path.clone(),
                        contents: Arc::from(contents.as_bytes()),
                        mode: 0o644,
                        owner: None,
                        expect: Expect::Absent,
                    },
                }]
            })())
        }
    }

    fn steps(last: &'static str) -> Vec<Box<dyn StepSpec>> {
        vec![
            Box::new(Ensure {
                key: "nix",
                path: "/nix".into(),
                contents: None,
            }),
            Box::new(Ensure {
                key: "var",
                path: "/nix/var".into(),
                contents: None,
            }),
            Box::new(Ensure {
                key: "marker",
                path: last.into(),
                contents: Some(""),
            }),
        ]
    }

    fn listing(root: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = walk(root)
            .into_iter()
            .map(|path| path.strip_prefix(root).unwrap().to_path_buf())
            .collect();
        found.sort();
        found
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(walk(&path));
            }
            found.push(path);
        }
        found
    }

    async fn run(
        root: &Path,
        last: &'static str,
        stopped: Stopped,
    ) -> (Report, Vec<mix_events::v1::Envelope>) {
        let outbox = Arc::new(Outbox::new("request", || {}));
        let mut tree = Tree::new(
            outbox.clone(),
            stopped.clone(),
            Start::command("bootstrap", Command::default()),
        );
        let mut runner = Runner::new(ROOT, steps(last));
        let mut performer = Performer::new(Files::open(root, "r1").unwrap());

        let mut journal = Vec::new();
        let report = drive(
            &mut runner,
            &mut tree,
            &mut performer,
            &Scope::root(),
            &stopped,
            &mut journal,
        )
        .await;
        assert_eq!(journal.last(), Some(&Record::Ended));
        drop(tree);
        (report, outbox.drain())
    }

    #[tokio::test]
    async fn a_plan_runs_on_a_real_file_system_and_commits() {
        let root = tempfile::tempdir().unwrap();

        let (report, stream) = run(root.path(), "/nix/.mix-managed", Arc::new(|| None)).await;

        assert_eq!(report.verdict, Verdict::Succeeded);
        assert!(root.path().join("nix/var").is_dir());
        assert!(root.path().join("nix/.mix-managed").is_file());
        assert!(validate(&stream).is_ok());
    }

    #[tokio::test]
    async fn a_failing_step_leaves_the_real_file_system_as_it_was() {
        let root = tempfile::tempdir().unwrap();
        let before = listing(root.path());

        let (report, stream) = run(root.path(), "/missing/parent/file", Arc::new(|| None)).await;

        assert!(matches!(
            report.verdict,
            Verdict::Failed { step: "marker", .. }
        ));
        assert_eq!(listing(root.path()), before);
        assert!(validate(&stream).is_ok());
    }

    #[tokio::test]
    async fn a_stop_between_steps_rolls_back_on_the_real_file_system() {
        let root = tempfile::tempdir().unwrap();
        let before = listing(root.path());
        let created = Arc::new(AtomicBool::new(false));
        let probe = created.clone();
        let dir = root.path().join("nix");
        let stopped: Stopped = Arc::new(move || {
            if dir.exists() {
                probe.store(true, Ordering::Relaxed);
            }
            probe
                .load(Ordering::Relaxed)
                .then_some(Cancellation::Interrupted)
        });

        let (report, stream) = run(root.path(), "/nix/.mix-managed", stopped).await;

        assert_eq!(
            report.verdict,
            Verdict::Cancelled(Cancellation::Interrupted)
        );
        assert!(created.load(Ordering::Relaxed));
        assert_eq!(listing(root.path()), before);
        assert!(validate(&stream).is_ok());
    }

    #[test]
    fn download_progress_becomes_byte_snapshots_against_the_total() {
        let mut seen = Vec::new();
        {
            let mut report = |progress: Progress| seen.push(progress);
            let relay = Relay {
                report: Mutex::new(&mut report),
                total: AtomicU64::new(0),
                done: AtomicU64::new(0),
            };
            relay.set_total(10);
            relay.add(4);
            relay.add(6);
        }

        assert_eq!(
            seen,
            [
                Progress::Bytes(Bytes {
                    done: 4,
                    total: Some(10)
                }),
                Progress::Bytes(Bytes {
                    done: 10,
                    total: Some(10)
                })
            ]
        );
    }
}
