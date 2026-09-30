use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use std::sync::Mutex;

use std::sync::atomic::{AtomicU64, Ordering};
use tracing::Instrument;

use mix_core::action::{Action, Fact, Failure, Kind, Outcome, PathFacts, Performed, Query};
use mix_core::journal::Record;
use mix_core::paths::SYSTEMD_UNIT_DIR as UNIT_DIR;
use mix_core::paths::mix_state_dir;
use mix_core::plan::{Input, Next, Report, Runner, make_guard};
use mix_core::privilege::InvokingUser;
use mix_core::{ActivityReporter, BuildProgress, DownloadProgress};
use mix_events::v1::node_progress::Progress;
use mix_events::v1::{Builds, Bytes, Cancellation, Stream};
use mix_events::{Stopped, Tree, output};
use mix_exec::Scope;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::effect::files::{Files, Prepared};
use crate::effect::generations::{self, ProfileContext};
use crate::effect::home::{self, Agent};
use crate::effect::identity;
use crate::effect::runtime;
use crate::effect::units::Units;
use crate::profile;

#[derive(Default)]
struct Activity {
    sender: Mutex<Option<UnboundedSender<Progress>>>,
}

impl Activity {
    fn connect(&self, sender: Option<UnboundedSender<Progress>>) {
        if let Ok(mut connected) = self.sender.lock() {
            *connected = sender;
        }
    }

    fn sender(&self) -> Option<UnboundedSender<Progress>> {
        self.sender.lock().ok()?.clone()
    }

    fn send(&self, progress: Progress) {
        if let Ok(connected) = self.sender.lock()
            && let Some(sender) = connected.as_ref()
        {
            let _ = sender.send(progress);
        }
    }
}

impl ActivityReporter for Activity {
    fn line(&self, line: &str) {
        self.send(output(line.as_bytes(), Stream::Stderr));
    }

    fn progress(&self, progress: &BuildProgress) {
        self.send(Progress::Builds(Builds {
            builds_done: progress.builds_done,
            builds_expected: progress.builds_expected,
            builds_running: progress.builds_running,
            downloads_done: progress.downloads_done,
            downloads_expected: progress.downloads_expected,
            downloads_running: progress.downloads_running,
            bytes_done: progress.bytes_done,
            bytes_expected: progress.bytes_expected,
        }));
    }

    fn clear(&self) {}
}

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
    agents: BTreeMap<u32, Agent>,
    units: Option<Units>,
    profile: Option<ProfileContext>,
    agent_program: Option<PathBuf>,
    activity: std::sync::Arc<Activity>,
    listening: Option<UnboundedReceiver<Progress>>,
}

impl Performer {
    pub fn new(files: Files) -> Self {
        Self {
            files,
            agents: BTreeMap::new(),
            units: None,
            profile: None,
            activity: std::sync::Arc::default(),
            listening: None,
            agent_program: None,
        }
    }

    pub fn with_agent_program(mut self, program: PathBuf) -> Self {
        self.agent_program = Some(program);
        self
    }

    fn agent(&mut self, uid: u32, path: &Path, scope: &Scope) -> Result<&mut Agent, Failure> {
        if !self.agents.contains_key(&uid) {
            let program = match &self.agent_program {
                Some(program) => program.clone(),
                None => home::myself()?,
            };
            let agent = Agent::spawn(
                &program,
                &home::account(uid, path)?,
                self.files.request(),
                scope,
            )?;
            self.agents.insert(uid, agent);
        }
        Ok(self.agents.get_mut(&uid).expect("inserted above"))
    }

    async fn perform_file(
        &mut self,
        action: &Action,
        path: &Path,
        scope: &Scope,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let removed = match action {
            Action::RemoveCreated { .. } | Action::RemoveCreatedTree { .. } => {
                self.files.owner_of(path)
            }
            _ => None,
        };
        let running = nix::unistd::geteuid().as_raw();
        match removed
            .or_else(|| self.files.tree_owner(path))
            .filter(|uid| *uid != running)
        {
            Some(uid) => {
                let agent = self.agent(uid, path, scope)?;
                Box::pin(agent.perform(action, prepared)).await
            }
            None => self.files.perform(action, prepared).expect("a file action"),
        }
    }

    async fn reclaim_by_copy(
        &mut self,
        path: &Path,
        expect: mix_core::action::FileId,
        owner: mix_core::action::Owner,
        mode: u32,
        scope: &Scope,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let staging = path.with_file_name(format!(".{name}.mix-reclaim-{}", self.files.request()));
        let copy = Action::CopyTree {
            from: path.to_path_buf(),
            to: staging.clone(),
            owner,
            mode,
        };
        self.perform_file(&copy, &staging, scope, prepared).await?;
        let restore = Action::Restore {
            path: path.to_path_buf(),
            from: staging,
            expect: mix_core::action::Expect::Absent,
        };
        prepared(std::slice::from_ref(&restore))?;
        let mut announced = |_: &[Action]| Ok(());
        let removal = Action::RemoveCreatedTree {
            path: path.to_path_buf(),
            expect,
        };
        self.perform_file(&removal, path, scope, &mut announced)
            .await?;
        self.perform_file(&restore, path, scope, &mut announced)
            .await?;
        Ok(Performed { undo: Vec::new() })
    }

    async fn commit(&mut self, prepared: &mut Prepared<'_>) -> Outcome {
        let mut first = None;
        for agent in self.agents.values_mut() {
            if let Err(failure) = agent.perform(&Action::Commit, prepared).await {
                first.get_or_insert(failure);
            }
        }
        let root = self
            .files
            .perform(&Action::Commit, prepared)
            .expect("a file action");
        match first {
            Some(failure) => Err(failure),
            None => root,
        }
    }

    pub fn with_profile(mut self, profile: ProfileContext) -> Self {
        self.profile = Some(profile);
        self
    }

    async fn record(
        &mut self,
        user: &InvokingUser,
        prepared: &mut Prepared<'_>,
        scope: &Scope,
    ) -> Outcome {
        let git = mix_state_dir(&user.home).join(".git");
        let existed = !matches!(
            self.files.observe(&Query::Path(git.clone())),
            Some(Fact::Path(PathFacts {
                kind: Kind::Missing,
                ..
            }))
        );
        prepared(&[])?;
        let host = self
            .profile
            .as_ref()
            .map(|profile| profile.host.clone())
            .unwrap_or_default();
        profile::record(user, &host, scope).await;
        let undo = match self.files.observe(&Query::Path(git.clone())) {
            Some(Fact::Path(PathFacts { id: Some(id), .. })) if !existed => {
                vec![Action::RemoveCreatedTree {
                    path: git,
                    expect: id,
                }]
            }
            _ => Vec::new(),
        };
        Ok(Performed { undo })
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
                    .or_else(|| generations::observe(query))
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
                return Box::pin(runtime::install(
                    url, sha256, *size, &relay, scope, prepared,
                ))
                .await;
            }
            Action::RemoveRuntime { created, kept } => {
                return Box::pin(runtime::remove(created, kept)).await;
            }
            Action::RecordState { user } => {
                return Box::pin(self.record(user, prepared, scope)).await;
            }
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
            Action::Commit => return Box::pin(self.commit(prepared)).await,
            _ => {}
        }
        if let Some(path) = home::path_of(action) {
            let outcome = self.perform_file(action, path, scope, prepared).await;
            return match (action, outcome) {
                (
                    Action::ReclaimTree {
                        path,
                        expect,
                        owner,
                        mode,
                    },
                    Err(Failure::Io {
                        kind: std::io::ErrorKind::CrossesDevices,
                        ..
                    }),
                ) => {
                    Box::pin(self.reclaim_by_copy(path, *expect, *owner, *mode, scope, prepared))
                        .await
                }
                (Action::ReclaimTree { .. }, Ok(performed)) => {
                    let asides = performed
                        .undo
                        .iter()
                        .filter_map(|undo| match undo {
                            Action::Restore { from, .. } => Some(from.clone()),
                            _ => None,
                        })
                        .collect();
                    Box::pin(self.adopt(asides, scope)).await?;
                    Ok(performed)
                }
                (_, outcome) => outcome,
            };
        }
        if let Some(outcome) = Box::pin(identity::perform(action, scope, prepared)).await {
            return outcome;
        }
        if let Some(profile) = &self.profile {
            let activity: std::sync::Arc<dyn ActivityReporter> = self.activity.clone();
            if let Some(outcome) = Box::pin(generations::perform(
                action, profile, &activity, scope, prepared,
            ))
            .await
            {
                return outcome;
            }
        }
        let units = Box::pin(self.units()).await?;
        if let Some(outcome) = Box::pin(units.perform(action, scope, prepared)).await {
            return outcome;
        }
        Err(Failure::CommandFailed {
            program: "mix".to_string(),
            status: None,
            output_tail: format!("no performer for {action:?}"),
        })
    }

    pub async fn adopt(&mut self, pending: Vec<PathBuf>, scope: &Scope) -> Result<(), Failure> {
        let mut theirs: BTreeMap<u32, Vec<PathBuf>> = BTreeMap::new();
        let mut ours = Vec::new();
        let running = nix::unistd::geteuid().as_raw();
        for path in pending {
            match self
                .files
                .owner_of(&path)
                .or(home::owner(&self.files, &path))
            {
                Some(uid) if uid != running => theirs.entry(uid).or_default().push(path),
                _ => ours.push(path),
            }
        }
        self.files.adopt(ours);
        for (uid, paths) in theirs {
            let first = paths[0].clone();
            self.agent(uid, &first, scope)?.adopt(paths).await?;
        }
        Ok(())
    }
}

pub trait Observer: Send {
    fn flush(&mut self);
    fn span(&self, node: mix_events::NodeId) -> Option<tracing::Span>;

    fn woken(&self) -> Option<std::sync::Arc<tokio::sync::Notify>> {
        None
    }
}

impl Observer for () {
    fn flush(&mut self) {}

    fn span(&self, _node: mix_events::NodeId) -> Option<tracing::Span> {
        None
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
        tracing::warn!(?record, ?failure, "could not record in the journal");
    }
}

pub fn stopped_by(scope: &Scope) -> Stopped {
    let watched = scope.clone();
    std::sync::Arc::new(move || match watched.reason()? {
        mix_exec::Reason::Interrupted => Some(Cancellation::Interrupted),
        mix_exec::Reason::Terminated => Some(Cancellation::Terminated),
        mix_exec::Reason::ClientGone => Some(Cancellation::ClientGone),
        mix_exec::Reason::Abandoned => None,
    })
}

pub async fn drive<'r>(
    runner: &'r mut Runner,
    tree: &mut Tree,
    performer: &mut Performer,
    scope: &Scope,
    stopped: &Stopped,
    journal: &mut dyn Journal,
    observer: &mut dyn Observer,
) -> &'r mut Report {
    make_guard!(guard);
    let mut runner = runner.brand(guard);
    let (sender, mut receiver) = match (performer.activity.sender(), performer.listening.take()) {
        (Some(sender), Some(receiver)) => (sender, receiver),
        _ => {
            let (sender, receiver) = unbounded_channel();
            performer.activity.connect(Some(sender.clone()));
            (sender, receiver)
        }
    };
    let woken = observer.woken();
    let observing = observer.span(mix_events::ROOT);
    let mut input = None;
    let mut seq = 0;
    let mut ended = false;
    loop {
        if let Some(cause) = stopped() {
            runner.stop(cause);
        }
        let next = runner.step(tree, input.take());
        observer.flush();
        match next {
            Next::Observe(queries) => {
                let facts = match &observing {
                    Some(span) => performer.observe(&queries).instrument(span.clone()).await,
                    None => performer.observe(&queries).await,
                };
                input = Some(Input::Facts(facts));
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
                let mut announced: Option<Vec<Action>> = None;
                let outcome = {
                    let mut progress = |progress: Progress| {
                        let _ = sender.send(progress);
                    };
                    let mut prepared = |undo: &[Action]| {
                        if reverting || committing {
                            return Ok(());
                        }
                        announced = Some(undo.to_vec());
                        journal.append(&Record::Prepared {
                            seq: this,
                            undo: undo.to_vec(),
                        })
                    };
                    let span = node
                        .and_then(|node| observer.span(node))
                        .unwrap_or_else(tracing::Span::none);
                    let performing = performer
                        .perform(&action, &scope, &mut progress, &mut prepared)
                        .instrument(span);
                    let mut performing = std::pin::pin!(performing);
                    loop {
                        tokio::select! {
                            outcome = &mut performing => break outcome,
                            Some(progress) = receiver.recv() => {
                                if let Some(node) = node {
                                    let _ = tree.progress(node, progress);
                                }
                                observer.flush();
                            }
                            () = async {
                                if let Some(woken) = &woken {
                                    woken.notified().await;
                                }
                            }, if woken.is_some() => observer.flush(),
                        }
                    }
                };
                while let Ok(progress) = receiver.try_recv() {
                    if let Some(node) = node {
                        let _ = tree.progress(node, progress);
                    }
                }
                observer.flush();
                match (&outcome, reverting, committing) {
                    (Ok(_), true, _) => keep(journal, &Record::Reverted { action }),
                    (Ok(_), false, true) => {
                        keep(journal, &Record::Ended);
                        ended = true;
                    }
                    (Ok(performed), false, false) => {
                        let record = if announced.as_deref() == Some(&performed.undo[..]) {
                            Record::Done { seq: this }
                        } else {
                            Record::Settled {
                                seq: this,
                                undo: performed.undo.clone(),
                            }
                        };
                        keep(journal, &record);
                        seq += 1;
                    }
                    (Err(_), false, false) => {
                        if let Some(undo) = announced.take() {
                            runner.in_doubt(undo);
                        }
                        seq += 1;
                    }
                    (Err(_), _, _) => {}
                }
                input = Some(Input::Done(outcome));
            }
            Next::Finished(closed) => {
                performer.listening = Some(receiver);
                if !ended {
                    keep(journal, &Record::Ended);
                }
                return runner.report(closed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use mix_core::action::{Expect, Kind};
    use mix_core::plan::{StepSpec, Verdict};
    use mix_events::v1::{Cancellation, Command};
    use mix_events::{Outbox, ROOT, Start, validate};

    use super::*;

    #[test]
    fn nix_output_is_sent_only_while_a_plan_is_listening() {
        let activity = Activity::default();
        activity.line("before");
        let (sender, mut receiver) = unbounded_channel();
        activity.connect(Some(sender));

        activity.line("building hello");
        activity.progress(&BuildProgress {
            builds_done: 1,
            builds_expected: 2,
            ..BuildProgress::default()
        });
        activity.connect(None);
        activity.line("after");

        assert_eq!(
            receiver.try_recv().ok(),
            Some(output(b"building hello", Stream::Stderr))
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(Progress::Builds(Builds {
                builds_done: 1,
                builds_expected: 2,
                ..
            }))
        ));
        assert!(receiver.try_recv().is_err());
    }

    struct Ensure {
        key: &'static str,
        path: PathBuf,
        contents: Option<&'static str>,
    }

    impl StepSpec for Ensure {
        fn key(&self) -> Cow<'static, str> {
            self.key.into()
        }

        fn title(&self) -> Cow<'static, str> {
            "ensure".into()
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
            &mut (),
        )
        .await;
        assert_eq!(journal.last(), Some(&Record::Ended));
        drop(tree);
        let report = report.clone();
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
            &report.verdict,
            Verdict::Failed { step, .. } if step == "marker"
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
