use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use std::sync::Mutex;

use std::sync::atomic::{AtomicU64, Ordering};

use mix_core::action::{
    Action, Fact, Failure, Kind, Outcome, PathFacts, Performed, Query, Subject,
};
use mix_core::identity::InvokingUser;
use mix_core::journal::Record;
use mix_core::paths::SYSTEMD_UNIT_DIR as UNIT_DIR;
use mix_core::paths::{mix_state_dir, repository_dir};
use mix_core::plan::{Input, Next, Report, Runner, make_guard};
use mix_core::world::World;
use mix_core::{ActivityReporter, BuildProgress, DownloadProgress};
use mix_events::v1::node_progress::Progress;
use mix_events::v1::{
    BuildStarted, Builds, Bytes, Cancellation, Code, CommandFinished, CommandStarted, Diagnostic,
    FetchStarted, LockWait, Stream, SubstitutionStarted,
};
use mix_events::{NodeId, ROOT, Stopped, Tree, output};
use mix_exec::Scope;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::effect::files::{Files, Prepared};
use crate::effect::generations::{self, ProfileContext, core_failure};
use crate::effect::git::Git;
use crate::effect::home::{self, Agent};
use crate::effect::identity;
use crate::effect::runtime;
use crate::effect::units::Units;
use crate::profile;

#[derive(Debug, PartialEq)]
pub enum Signal {
    Progress(Progress),
    Warning(Box<Diagnostic>),
}

fn announce(announced: &mut bool, tree: &mut Tree, cause: Option<Cancellation>) {
    if *announced {
        return;
    }
    *announced = true;
    if let Some(cause) = cause {
        let _ = tree.progress(
            ROOT,
            Progress::Stopping(mix_events::v1::Stopping {
                cause: cause as i32,
            }),
        );
    }
}

fn apply(tree: &mut Tree, node: NodeId, signal: Signal) {
    let _ = match signal {
        Signal::Progress(progress) => tree.progress(node, progress),
        Signal::Warning(diagnostic) => tree.warn(node, *diagnostic),
    };
}

#[derive(Default)]
struct Activity {
    sender: Mutex<Option<UnboundedSender<Signal>>>,
}

impl Activity {
    fn connect(&self, sender: Option<UnboundedSender<Signal>>) {
        if let Ok(mut connected) = self.sender.lock() {
            *connected = sender;
        }
    }

    fn sender(&self) -> Option<UnboundedSender<Signal>> {
        self.sender.lock().ok()?.clone()
    }

    fn send(&self, progress: Progress) {
        self.signal(Signal::Progress(progress));
    }

    fn signal(&self, signal: Signal) {
        if let Ok(connected) = self.sender.lock()
            && let Some(sender) = connected.as_ref()
        {
            let _ = sender.send(signal);
        }
    }
}

impl mix_exec::Watch for Activity {
    fn started(&self, command: &str) {
        self.send(Progress::Command(CommandStarted {
            line: command.to_string(),
        }));
    }

    fn finished(&self, command: &str, status: std::process::ExitStatus) {
        use std::os::unix::process::ExitStatusExt;

        self.send(Progress::CommandFinished(CommandFinished {
            line: command.to_string(),
            exit_code: status.code(),
            signal: status.signal(),
        }));
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

    fn build_started(&self, derivation: &str) {
        self.send(Progress::Build(BuildStarted {
            derivation: derivation.to_string(),
            name: mix_core::nix_log::package_name(derivation).to_string(),
        }));
    }

    fn fetch_started(&self, path: &str) {
        self.send(Progress::Substitution(SubstitutionStarted {
            path: path.to_string(),
            name: mix_core::nix_log::package_name(path).to_string(),
        }));
    }

    fn waiting(&self, lock: &str, holder: Option<&str>, command: Option<&str>) {
        self.send(Progress::Waiting(LockWait {
            lock: lock.to_string(),
            holder: holder.map(str::to_string),
            command: command.map(str::to_string),
        }));
    }
}

pub(crate) struct Relay<'a> {
    report: Mutex<&'a mut (dyn FnMut(Signal) + Send)>,
    total: AtomicU64,
    done: AtomicU64,
}

impl Relay<'_> {
    pub(crate) fn warning(&self, diagnostic: Diagnostic) {
        if let Ok(mut report) = self.report.lock() {
            report(Signal::Warning(Box::new(diagnostic)));
        }
    }
}

impl DownloadProgress for Relay<'_> {
    fn fetching(&self, url: &str) {
        if let Ok(mut report) = self.report.lock() {
            report(Signal::Progress(Progress::Fetch(FetchStarted {
                url: url.to_string(),
            })));
        }
    }

    fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    fn add(&self, delta: u64) {
        let done = self.done.fetch_add(delta, Ordering::Relaxed) + delta;
        let total = self.total.load(Ordering::Relaxed);
        if let Ok(mut report) = self.report.lock() {
            report(Signal::Progress(Progress::Bytes(Bytes {
                done,
                total: (total > 0).then_some(total),
            })));
        }
    }
}

#[derive(Default)]
struct Prediction {
    world: mix_core::world::World,
    seeded: std::collections::HashSet<Subject>,
    touched: Vec<Subject>,
}

impl Prediction {
    fn answers(&self, query: &Query) -> bool {
        let near = |path: &Path| {
            self.touched.iter().any(|subject| {
                matches!(subject, Subject::Path(touched)
                    if touched.starts_with(path) || path.starts_with(touched))
            })
        };
        match query {
            Query::Path(path)
            | Query::Contents(path)
            | Query::TreeOwner(path)
            | Query::Leftovers(path)
            | Query::Strangers { path, .. }
            | Query::Program { path, .. } => near(path),
            Query::Repository(user) => near(&repository_dir(&user.home)),
            Query::Group(name) => self.touched.contains(&Subject::Group(name.clone())),
            Query::User(name) => self.touched.contains(&Subject::User(name.clone())),
            Query::Unit(name) => self.touched.contains(&Subject::Unit(name.clone())),
            Query::Profile(user) | Query::Clobbered(user) => {
                self.touched.contains(&Subject::Profile(user.clone()))
            }
            Query::Journals(_) => false,
        }
    }
}

pub struct Performer {
    files: Files,
    model: Option<std::sync::Arc<Mutex<World>>>,
    prediction: Option<Prediction>,
    agents: BTreeMap<u32, Agent>,
    units: Option<Units>,
    profile: Option<ProfileContext>,
    agent_program: Option<PathBuf>,
    activity: std::sync::Arc<Activity>,
    listening: Option<UnboundedReceiver<Signal>>,
}

impl Performer {
    pub fn new(files: Files) -> Self {
        Self {
            files,
            model: None,
            prediction: None,
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

    async fn find_built(
        &mut self,
        user: &mix_core::identity::InvokingUser,
        scope: &Scope,
    ) -> Result<Option<u64>, Failure> {
        let generations = generations::existing(user);
        if user.uid == nix::unistd::geteuid().as_raw() {
            return Ok(crate::profile::state::built_generation(
                &user.home,
                &generations,
            ));
        }
        self.agent(user.uid, &user.home, scope)?
            .find_built(&user.home, &generations)
            .await
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
        let tree_removal = matches!(action, Action::RemoveCreatedTree { .. });
        match removed
            .or_else(|| self.files.tree_owner(path))
            .filter(|uid| *uid != running)
            .filter(|uid| !(tree_removal && self.files.holds_other_than(path, *uid)))
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

    /// Sets `path` aside where a rename cannot, as for a directory in a lower overlayfs layer: the
    /// tree is copied aside, the original removed, and the copy removed when the request commits.
    async fn set_aside_by_copy(
        &mut self,
        path: &Path,
        expect: mix_core::action::FileId,
        scope: &Scope,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let Some(Fact::Path(found)) = self.files.observe(&Query::Path(path.to_path_buf())) else {
            return Err(Failure::Conflict {
                subject: path.display().to_string(),
                expected: format!("{expect:?}"),
                found: "nothing".to_string(),
            });
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let aside = path.with_file_name(format!(".{name}.mix-aside-{}", self.files.request()));
        let copy = Action::CopyTree {
            from: path.to_path_buf(),
            to: aside.clone(),
            owner: found.owner,
            mode: found.mode,
        };
        self.perform_file(&copy, &aside, scope, prepared).await?;
        let restore = Action::Restore {
            path: path.to_path_buf(),
            from: aside.clone(),
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
        Box::pin(self.adopt(vec![aside], scope)).await?;
        Ok(Performed {
            undo: vec![restore],
        })
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
        report: &mut (dyn FnMut(Signal) + Send),
        scope: &Scope,
    ) -> Outcome {
        let git = repository_dir(&user.home);
        let existed = !matches!(
            self.files.observe(&Query::Path(git.clone())),
            Some(Fact::Path(PathFacts {
                kind: Kind::Missing,
                ..
            }))
        );
        prepared(&[])?;
        if let Err(error) = profile::record(user, scope).await {
            report(Signal::Warning(Box::new(mix_core::diagnose::warning(
                Code::GitRecordFailed,
                "could not record the change in git",
                &error,
            ))));
        }
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

    async fn create_repository(
        &mut self,
        user: &InvokingUser,
        prepared: &mut Prepared<'_>,
        scope: &Scope,
    ) -> Outcome {
        let repository = repository_dir(&user.home);
        let found = self.files.observe(&Query::Path(repository.clone()));
        if !matches!(
            found,
            Some(Fact::Path(PathFacts {
                kind: Kind::Missing,
                ..
            }))
        ) {
            return Err(Failure::Conflict {
                subject: repository.display().to_string(),
                expected: "nothing".to_string(),
                found: "something already there".to_string(),
            });
        }
        prepared(&[])?;
        let created = Git::resolve(user)
            .await
            .create(user, &mix_state_dir(&user.home), scope)
            .await;
        let undo = match self.files.observe(&Query::Path(repository.clone())) {
            Some(Fact::Path(PathFacts { id: Some(id), .. })) => vec![Action::RemoveCreatedTree {
                path: repository.clone(),
                expect: id,
            }],
            _ => Vec::new(),
        };
        if let Err(error) = created {
            let mut announced = |_: &[Action]| Ok(());
            for removal in &undo {
                self.perform_file(removal, &repository, &scope.shielded(), &mut announced)
                    .await?;
            }
            return Err(core_failure(error));
        }
        Ok(Performed { undo })
    }

    pub fn modelled(world: std::sync::Arc<Mutex<World>>) -> std::io::Result<Self> {
        Ok(Self {
            model: Some(world),
            ..Self::new(Files::open(Path::new("/"), "model")?)
        })
    }

    fn modelled_world(&self) -> Option<std::sync::MutexGuard<'_, World>> {
        self.model.as_ref().map(|world| {
            world
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
    }

    pub fn predicting(files: Files) -> Self {
        Self {
            prediction: Some(Prediction::default()),
            ..Self::new(files)
        }
    }

    pub async fn observe(
        &mut self,
        queries: &[Query],
        scope: &Scope,
    ) -> Result<Vec<Fact>, Failure> {
        if let Some(world) = self.modelled_world() {
            return Ok(queries.iter().map(|query| world.observe(query)).collect());
        }
        let mut facts = Vec::with_capacity(queries.len());
        for query in queries {
            let fact = match &self.prediction {
                Some(prediction) if prediction.answers(query) => prediction.world.observe(query),
                _ => self.observe_one(query, scope).await?,
            };
            facts.push(fact);
        }
        Ok(facts)
    }

    async fn seed(&mut self, subject: &Subject, scope: &Scope) -> Result<(), Failure> {
        if self
            .prediction
            .as_ref()
            .is_none_or(|prediction| prediction.seeded.contains(subject))
        {
            return Ok(());
        }
        match subject {
            Subject::Path(path) => {
                let mut chain: Vec<&Path> = path
                    .ancestors()
                    .filter(|at| at.parent().is_some())
                    .collect();
                chain.reverse();
                for at in chain {
                    let at_subject = Subject::Path(at.to_path_buf());
                    if self
                        .prediction
                        .as_ref()
                        .is_some_and(|prediction| prediction.seeded.contains(&at_subject))
                    {
                        continue;
                    }
                    let Fact::Path(found) =
                        self.observe_one(&Query::Path(at.into()), scope).await?
                    else {
                        continue;
                    };
                    let contents = match found.kind {
                        Kind::File => {
                            match self.observe_one(&Query::Contents(at.into()), scope).await? {
                                Fact::Contents(contents) => contents,
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some(prediction) = &mut self.prediction {
                        prediction.world.seed_path(at, &found, contents);
                        prediction.seeded.insert(at_subject);
                    }
                }
                return Ok(());
            }
            Subject::Group(name) => {
                if let Fact::Group(found) =
                    self.observe_one(&Query::Group(name.clone()), scope).await?
                    && let Some(prediction) = &mut self.prediction
                {
                    prediction.world.seed_group(name, found);
                }
            }
            Subject::User(name) => {
                if let Fact::User(found) =
                    self.observe_one(&Query::User(name.clone()), scope).await?
                    && let Some(prediction) = &mut self.prediction
                {
                    prediction.world.seed_user(name, found);
                }
            }
            Subject::Unit(name) => {
                let file = Path::new(UNIT_DIR).join(name);
                Box::pin(self.seed(&Subject::Path(file.clone()), scope)).await?;
                let found = self.units().await?.observe(name).await?;
                let contents = match self.observe_one(&Query::Contents(file), scope).await? {
                    Fact::Contents(contents) => contents,
                    _ => None,
                };
                if let Some(prediction) = &mut self.prediction {
                    prediction.world.seed_unit(name, &found, contents);
                }
            }
            Subject::Profile(user) => {
                if let Fact::Profile(found) = self
                    .observe_one(&Query::Profile(user.clone()), scope)
                    .await?
                    && let Some(prediction) = &mut self.prediction
                {
                    prediction.world.seed_profile(user.uid, &found);
                }
            }
        }
        if let Some(prediction) = &mut self.prediction {
            prediction.seeded.insert(subject.clone());
        }
        Ok(())
    }

    async fn predict(
        &mut self,
        action: &Action,
        scope: &Scope,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let subjects = action.subjects();
        for subject in &subjects {
            self.seed(subject, scope).await?;
        }
        let prediction = self
            .prediction
            .as_mut()
            .expect("only a predicting performer predicts");
        let performed = prediction.world.apply(action)?;
        prepared(&performed.undo)?;
        prediction.touched.extend(subjects);
        Ok(performed)
    }

    async fn observe_one(&mut self, query: &Query, scope: &Scope) -> Result<Fact, Failure> {
        Ok({
            match query {
                Query::Unit(unit) => Fact::Unit(self.units().await?.observe(unit).await?),
                Query::Journals(dir) => Fact::Journals(crate::effect::journal::abandoned(dir)),
                Query::Program { path, source } => {
                    Fact::Program(crate::effect::program::observe(path, source))
                }
                Query::Repository(user) => {
                    let git = Git::resolve(user).await;
                    let intact = git
                        .verify(user, &repository_dir(&user.home), scope)
                        .await
                        .map_err(core_failure)?;
                    let recorded = intact
                        && git
                            .recorded(user, &mix_state_dir(&user.home), scope)
                            .await
                            .unwrap_or(false);
                    Fact::Repository { intact, recorded }
                }
                query => self
                    .files
                    .observe(query)
                    .or_else(|| identity::observe(query))
                    .or_else(|| generations::observe(query))
                    .expect("every query has an observer"),
            }
        })
    }

    pub async fn perform(
        &mut self,
        action: &Action,
        scope: &Scope,
        report: &mut (dyn FnMut(Signal) + Send),
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        if let Some(mut world) = self.modelled_world() {
            let undo = world.clone().apply(action)?.undo;
            prepared(&undo)?;
            return world.apply(action);
        }
        if self.prediction.is_some() {
            return Box::pin(self.predict(action, scope, prepared)).await;
        }
        match action {
            Action::InstallRuntime { url, sha256, size } => {
                let relay = Relay {
                    report: Mutex::new(report),
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
                return Box::pin(self.record(user, prepared, report, scope)).await;
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
            Action::CreateRepository { user } => {
                return Box::pin(self.create_repository(user, prepared, scope)).await;
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
                (
                    Action::SetAside { path, expect },
                    Err(Failure::Io {
                        kind: std::io::ErrorKind::CrossesDevices,
                        ..
                    }),
                ) => Box::pin(self.set_aside_by_copy(path, *expect, scope, prepared)).await,
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
        if self.profile.is_some() {
            let built = match action {
                Action::ActivateProfile { user, .. } => self.find_built(user, scope).await?,
                _ => None,
            };
            let profile = self.profile.as_ref().expect("checked above");
            let activity: std::sync::Arc<dyn ActivityReporter> = self.activity.clone();
            if let Some(outcome) = Box::pin(generations::perform(
                action, built, profile, &activity, scope, prepared,
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
                Some(uid) if uid != running && !self.files.holds_other_than(&path, uid) => {
                    theirs.entry(uid).or_default().push(path);
                }
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
    fn wants(&self, _detail: mix_events::Detail) -> bool {
        false
    }
}

impl Observer for () {}

pub trait Journal: Send {
    fn append(&mut self, record: &Record) -> Result<(), Failure>;
}

impl Journal for Vec<Record> {
    fn append(&mut self, record: &Record) -> Result<(), Failure> {
        self.push(record.clone());
        Ok(())
    }
}

fn journaled(record: &Record) -> Progress {
    Progress::Journaled(mix_core::trace::journaled(record))
}

fn keep(journal: &mut dyn Journal, record: &Record, tree: &mut Tree, node: NodeId, traced: bool) {
    match journal.append(record) {
        Ok(()) if traced => {
            let _ = tree.progress(node, journaled(record));
        }
        Ok(()) => {}
        Err(failure) => {
            let _ = tree.warn(
                node,
                mix_core::diagnose::warning(
                    Code::JournalUnwritable,
                    "could not record progress in the journal",
                    &failure,
                ),
            );
        }
    }
}

pub fn stopped_by(scope: &Scope) -> Stopped {
    let watched = scope.clone();
    std::sync::Arc::new(move || {
        watched.reason().map(|reason| match reason {
            mix_exec::Reason::Interrupted => Cancellation::Interrupted,
            mix_exec::Reason::Terminated => Cancellation::Terminated,
            mix_exec::Reason::ClientGone => Cancellation::ClientGone,
        })
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
    let scope = &scope.watched(performer.activity.clone());
    let mut stopping = false;
    let traced = observer.wants(mix_events::Detail::Trace);
    let mut input = None;
    let mut seq = 0;
    let mut ended = false;
    loop {
        if let Some(cause) = stopped() {
            announce(&mut stopping, tree, Some(cause));
            runner.stop(cause);
        }
        let next = runner.step(tree, input.take());
        match next {
            Next::Observe(queries) => {
                let facts = performer.observe(&queries, scope).await;
                if traced && let Ok(found) = &facts {
                    let _ = tree.progress(
                        runner.current_node().unwrap_or(ROOT),
                        Progress::Observed(mix_core::trace::observed(&queries, found)),
                    );
                }
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
                if committing {
                    if let Err(failure) = journal.append(&Record::Committing) {
                        input = Some(Input::Done(Err(failure)));
                        continue;
                    }
                    if traced {
                        let _ = tree.progress(node.unwrap_or(ROOT), journaled(&Record::Committing));
                    }
                }
                let this = seq;
                let mut announced: Option<Vec<Action>> = None;
                let outcome = {
                    let mut report = |signal: Signal| {
                        let _ = sender.send(signal);
                    };
                    let mut prepared = |undo: &[Action]| {
                        if reverting || committing {
                            return Ok(());
                        }
                        announced = Some(undo.to_vec());
                        let record = Record::Prepared {
                            seq: this,
                            undo: undo.to_vec(),
                        };
                        journal.append(&record)?;
                        if traced {
                            let _ = sender.send(Signal::Progress(journaled(&record)));
                        }
                        Ok(())
                    };
                    let performing = performer.perform(&action, &scope, &mut report, &mut prepared);
                    let mut performing = std::pin::pin!(performing);
                    loop {
                        tokio::select! {
                            biased;
                            outcome = &mut performing => break outcome,
                            Some(signal) = receiver.recv() => {
                                apply(tree, node.unwrap_or(ROOT), signal);
                            }
                            _ = scope.stopped(), if !stopping => {
                                announce(&mut stopping, tree, stopped());
                            }
                        }
                    }
                };
                let acting = node.unwrap_or(ROOT);
                while let Ok(signal) = receiver.try_recv() {
                    apply(tree, acting, signal);
                }
                match (&outcome, reverting, committing) {
                    (Ok(_), true, _) => {
                        keep(journal, &Record::Reverted { action }, tree, acting, traced)
                    }
                    (Ok(_), false, true) => {
                        keep(journal, &Record::Ended, tree, acting, traced);
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
                        keep(journal, &record, tree, acting, traced);
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
                    keep(journal, &Record::Ended, tree, ROOT, traced);
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
            Some(Signal::Progress(output(b"building hello", Stream::Stderr)))
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(Signal::Progress(Progress::Builds(Builds {
                builds_done: 1,
                builds_expected: 2,
                ..
            })))
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn a_command_a_build_and_a_warning_reach_the_node_that_is_acting() {
        let activity = Activity::default();
        let (sender, mut receiver) = unbounded_channel();
        activity.connect(Some(sender));
        mix_exec::Watch::started(&activity, "nix build .#hello");
        activity.build_started("/nix/store/x-hello.drv");
        activity.signal(Signal::Warning(Box::new(mix_core::diagnose::warning(
            Code::GitRecordFailed,
            "could not record the change in git",
            &Failure::Cancelled,
        ))));

        let outbox = Arc::new(Outbox::new("request", || {}));
        let mut tree = Tree::new(
            Arc::clone(&outbox),
            Arc::new(|| None),
            Start::command("install", Command::default()),
        );
        while let Ok(signal) = receiver.try_recv() {
            apply(&mut tree, ROOT, signal);
        }
        tree.finish(ROOT, mix_events::Ending::succeeded()).unwrap();

        let stream = outbox.drain();
        assert!(validate(&stream).is_ok());
        let events: Vec<_> = stream.into_iter().filter_map(|e| e.event).collect();
        use mix_events::v1::envelope::Event;
        assert!(matches!(
            &events[1],
            Event::NodeProgress(p) if p.progress == Some(Progress::Command(CommandStarted { line: "nix build .#hello".into() }))
        ));
        assert!(matches!(
            &events[2],
            Event::NodeProgress(p) if p.progress == Some(Progress::Build(BuildStarted { derivation: "/nix/store/x-hello.drv".into(), name: "x-hello".into() }))
        ));
        assert!(matches!(
            &events[3],
            Event::Diagnostic(d) if d.node == ROOT && d.code() == Code::GitRecordFailed
        ));
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

        fn title(&self) -> mix_core::plan::Title {
            mix_core::plan::Title::new(mix_events::v1::Verb::Creating, "ensure")
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

    struct Everything;

    impl Observer for Everything {
        fn wants(&self, _detail: mix_events::Detail) -> bool {
            true
        }
    }

    async fn run(
        root: &Path,
        last: &'static str,
        stopped: Stopped,
    ) -> (Report, Vec<mix_events::v1::Envelope>) {
        run_for(root, last, stopped, &mut ()).await
    }

    async fn run_for(
        root: &Path,
        last: &'static str,
        stopped: Stopped,
        observer: &mut dyn Observer,
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
            observer,
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

    fn kinds(stream: &[mix_events::v1::Envelope]) -> (usize, usize) {
        stream
            .iter()
            .fold((0, 0), |(observed, journaled), envelope| {
                match &envelope.event {
                    Some(mix_events::v1::envelope::Event::NodeProgress(progress)) => {
                        match &progress.progress {
                            Some(Progress::Observed(_)) => (observed + 1, journaled),
                            Some(Progress::Journaled(_)) => (observed, journaled + 1),
                            _ => (observed, journaled),
                        }
                    }
                    _ => (observed, journaled),
                }
            })
    }

    #[tokio::test]
    async fn what_the_engine_saw_and_journaled_is_built_only_for_someone_who_wants_it() {
        let traced = tempfile::tempdir().unwrap();
        let (_, stream) = run_for(
            traced.path(),
            "/nix/.mix-managed",
            Arc::new(|| None),
            &mut Everything,
        )
        .await;
        assert!(validate(&stream).is_ok());
        let (observed, journaled) = kinds(&stream);
        assert!(observed > 0);
        assert!(journaled > 0);

        let quiet = tempfile::tempdir().unwrap();
        let (_, stream) = run(quiet.path(), "/nix/.mix-managed", Arc::new(|| None)).await;
        assert_eq!(kinds(&stream), (0, 0));
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
        let announced: Vec<_> = stream
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Some(mix_events::v1::envelope::Event::NodeProgress(progress)) => {
                    match &progress.progress {
                        Some(Progress::Stopping(stopping)) => Some((progress.id, stopping.cause())),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        assert_eq!(announced, [(ROOT, Cancellation::Interrupted)]);
    }

    #[test]
    fn download_progress_becomes_byte_snapshots_against_the_total() {
        let mut seen = Vec::new();
        {
            let mut report = |signal: Signal| seen.push(signal);
            let relay = Relay {
                report: Mutex::new(&mut report),
                total: AtomicU64::new(0),
                done: AtomicU64::new(0),
            };
            relay.fetching("http://mirror.internal/nix.tar.xz");
            relay.set_total(10);
            relay.add(4);
            relay.add(6);
        }

        assert_eq!(
            seen,
            [
                Signal::Progress(Progress::Fetch(FetchStarted {
                    url: "http://mirror.internal/nix.tar.xz".into()
                })),
                Signal::Progress(Progress::Bytes(Bytes {
                    done: 4,
                    total: Some(10)
                })),
                Signal::Progress(Progress::Bytes(Bytes {
                    done: 10,
                    total: Some(10)
                }))
            ]
        );
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    #[ignore = "requires git"]
    async fn a_change_git_cannot_record_still_takes_effect_and_says_so() {
        let home = tempfile::tempdir().unwrap();
        let user = crate::effect::git::testing::user(home.path(), Some("commit"));
        let state_dir = mix_state_dir(&user.home);
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join(mix_core::paths::FLAKE_NIX), "flake-content").unwrap();
        let mut performer = Performer::new(Files::open(Path::new("/"), "r1").unwrap());
        let mut signals = Vec::new();

        let outcome = performer
            .perform(
                &Action::RecordState { user: user.clone() },
                &Scope::root(),
                &mut |signal| signals.push(signal),
                &mut |_: &[Action]| Ok(()),
            )
            .await;

        assert!(outcome.is_ok(), "{outcome:?}");
        assert!(matches!(
            signals.as_slice(),
            [Signal::Warning(warning)] if warning.code() == Code::GitRecordFailed
        ));
    }

    #[tokio::test]
    async fn the_running_program_is_compared_through_its_proc_link() {
        let mut performer = Performer::new(Files::open(Path::new("/"), "r1").unwrap());
        let running = std::env::current_exe().unwrap();

        let facts = performer
            .observe(
                &[Query::Program {
                    path: running,
                    source: mix_core::paths::RUNNING_PROGRAM.into(),
                }],
                &Scope::root(),
            )
            .await
            .unwrap();

        assert_eq!(
            facts,
            [Fact::Program(mix_core::action::ProgramFacts {
                same: true,
                source: None
            })]
        );
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_prediction_answers_from_what_it_predicted_and_the_machine_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("etc")).unwrap();
        std::fs::write(root.path().join("etc/nix.conf"), "legacy\n").unwrap();
        let before = listing(root.path());
        let me = nix::unistd::Uid::current().as_raw();
        let mut performer =
            Performer::predicting(Files::open_trusting(root.path(), "r1", me).unwrap());
        let scope = Scope::root();
        let Fact::Path(found) = performer
            .observe(&[Query::Path("/etc/nix.conf".into())], &scope)
            .await
            .unwrap()
            .remove(0)
        else {
            panic!("a path is observed as a path");
        };
        let mut undone = Vec::new();

        for action in [
            Action::PutFile {
                path: "/etc/nix.conf".into(),
                contents: Arc::from(&b"trusted-users = root\n"[..]),
                mode: 0o644,
                owner: None,
                expect: Expect::Present(found.id.unwrap()),
            },
            Action::CreateDirs {
                path: "/nix/var".into(),
                mode: 0o755,
                owner: None,
            },
        ] {
            let performed = performer
                .perform(&action, &scope, &mut |_| {}, &mut |undo: &[Action]| {
                    undone.extend_from_slice(undo);
                    Ok(())
                })
                .await
                .unwrap();
            assert!(!performed.undo.is_empty(), "{action:?}");
        }
        let facts = performer
            .observe(
                &[
                    Query::Contents("/etc/nix.conf".into()),
                    Query::Path("/nix/var".into()),
                ],
                &scope,
            )
            .await
            .unwrap();

        assert_eq!(
            facts[0],
            Fact::Contents(Some(Arc::from(&b"trusted-users = root\n"[..])))
        );
        assert!(matches!(
            &facts[1],
            Fact::Path(PathFacts {
                kind: Kind::Directory,
                ..
            })
        ));
        assert_eq!(listing(root.path()), before);
        assert_eq!(
            std::fs::read_to_string(root.path().join("etc/nix.conf")).unwrap(),
            "legacy\n"
        );
        assert!(!undone.is_empty());
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_prediction_refuses_what_the_machine_would_refuse() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("etc")).unwrap();
        std::fs::write(root.path().join("etc/nix.conf"), "legacy\n").unwrap();
        let me = nix::unistd::Uid::current().as_raw();
        let mut performer =
            Performer::predicting(Files::open_trusting(root.path(), "r1", me).unwrap());

        let refused = performer
            .perform(
                &Action::PutFile {
                    path: "/etc/nix.conf".into(),
                    contents: Arc::from(&b"x"[..]),
                    mode: 0o644,
                    owner: None,
                    expect: Expect::Absent,
                },
                &Scope::root(),
                &mut |_| {},
                &mut |_: &[Action]| Ok(()),
            )
            .await;

        assert!(
            matches!(refused, Err(Failure::Conflict { .. })),
            "{refused:?}"
        );
    }
}
