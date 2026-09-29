use std::collections::VecDeque;

use mix_events::v1::{
    Action as ActionNode, Cancellation, Code, CommandDetail, Diagnostic, IntegrityDetail, IoDetail,
    NetworkDetail, Operation, Plan, Rollback, Severity, Step, StepsDetail, diagnostic::Detail,
    node_started::Kind,
};
use mix_events::{Ending, NodeId, Start, Tree};

use crate::action::{Action, Fact, Failure, Outcome, Query, rollback_order};

pub trait StepSpec: Send + Sync {
    fn key(&self) -> &'static str;
    fn title(&self) -> &'static str;
    fn queries(&self) -> Vec<Query>;
    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure>;

    fn shielded(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Facts(Result<Vec<Fact>, Failure>),
    Done(Outcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Observe(Vec<Query>),
    Perform(Action),
    Finished(Report),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Succeeded,
    Failed {
        step: &'static str,
        failure: Failure,
    },
    Cancelled(Cancellation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub verdict: Verdict,
    pub rollback_failures: Vec<(&'static str, Failure)>,
}

struct Doubt {
    step: usize,
    undo: Vec<Action>,
}

struct Undoing {
    step: usize,
    node: NodeId,
    doubtful: VecDeque<Action>,
    queue: VecDeque<Action>,
    doubting: bool,
    failed: bool,
}

enum Phase {
    Opening,
    Checking {
        step: usize,
        asked: bool,
    },
    Executing {
        step: usize,
        node: NodeId,
        queue: VecDeque<Action>,
        awaiting: bool,
    },
    RollingBack {
        remaining: Vec<usize>,
        current: Option<Undoing>,
        awaiting: bool,
    },
    Committing {
        awaiting: bool,
    },
    Closing,
    Closed(Report),
}

pub struct Runner {
    steps: Vec<Box<dyn StepSpec>>,
    parent: NodeId,
    plan: NodeId,
    journal: Vec<Vec<Vec<Action>>>,
    doubts: Vec<Doubt>,
    nodes: Vec<Option<NodeId>>,
    phase: Phase,
    stop: Option<Cancellation>,
    verdict: Option<Verdict>,
    rollback_failures: Vec<(&'static str, Failure)>,
    acting: Option<NodeId>,
    performed: u64,
}

impl Runner {
    pub fn new(parent: NodeId, steps: Vec<Box<dyn StepSpec>>) -> Self {
        let count = steps.len();
        Self {
            steps,
            parent,
            plan: 0,
            journal: vec![Vec::new(); count],
            doubts: Vec::new(),
            nodes: vec![None; count],
            phase: Phase::Opening,
            stop: None,
            verdict: None,
            rollback_failures: Vec::new(),
            acting: None,
            performed: 0,
        }
    }

    fn act(&mut self, tree: &mut Tree, parent: NodeId, action: Action) -> Next {
        self.performed += 1;
        let (operation, subject) = describe(&action);
        self.acting = Some(
            tree.start(
                parent,
                Start::new(
                    format!("action-{}", self.performed),
                    Kind::Action(ActionNode {
                        operation: operation as i32,
                        subject,
                    }),
                ),
            )
            .expect("an action starts under a running node"),
        );
        Next::Perform(action)
    }

    fn acted(&mut self, tree: &mut Tree, outcome: &Outcome, cause: Option<Cancellation>) {
        let Some(node) = self.acting.take() else {
            return;
        };
        let ending = match outcome {
            Ok(_) => Ending::succeeded(),
            Err(Failure::Cancelled) => {
                Ending::cancelled(cause.unwrap_or(Cancellation::Interrupted))
            }
            Err(failure) => Ending::failed(diagnostic(failure)),
        };
        finish(tree, node, ending);
    }

    pub fn current_node(&self) -> Option<NodeId> {
        if self.acting.is_some() {
            return self.acting;
        }
        match &self.phase {
            Phase::Executing { node, .. } => Some(*node),
            Phase::RollingBack {
                current: Some(undoing),
                ..
            } => Some(undoing.node),
            _ => None,
        }
    }

    pub fn rolling_back(&self) -> bool {
        matches!(self.phase, Phase::RollingBack { .. })
    }

    pub fn shielded(&self) -> bool {
        match &self.phase {
            Phase::RollingBack { .. } | Phase::Committing { .. } => true,
            Phase::Executing { step, .. } => self.steps[*step].shielded(),
            _ => false,
        }
    }

    pub fn in_doubt(&mut self, undo: Vec<Action>) {
        if let Phase::Executing { step, .. } = self.phase
            && !undo.is_empty()
        {
            self.doubts.push(Doubt { step, undo });
        }
    }

    pub fn stop(&mut self, cause: Cancellation) {
        self.stop.get_or_insert(cause);
    }

    pub fn step(&mut self, tree: &mut Tree, input: Option<Input>) -> Next {
        let mut input = input;
        loop {
            match std::mem::replace(&mut self.phase, Phase::Closing) {
                Phase::Opening => {
                    let keys: Vec<&'static str> =
                        self.steps.iter().map(|step| step.key()).collect();
                    self.plan = tree
                        .start(
                            self.parent,
                            Start::new("plan", Kind::Plan(Plan::default())).planned(keys),
                        )
                        .expect("the plan starts under a running parent");
                    self.phase = Phase::Checking {
                        step: 0,
                        asked: false,
                    };
                }
                Phase::Checking { step, asked: false } => {
                    if step == self.steps.len() {
                        if self.journal.iter().all(Vec::is_empty) {
                            self.verdict = Some(Verdict::Succeeded);
                            self.phase = Phase::Closing;
                        } else {
                            self.phase = Phase::Committing { awaiting: false };
                        }
                    } else if let Some(cause) = self.stop {
                        self.cancel(cause);
                    } else {
                        self.phase = Phase::Checking { step, asked: true };
                        return Next::Observe(self.steps[step].queries());
                    }
                }
                Phase::Checking { step, asked: true } => {
                    let Some(Input::Facts(facts)) = input.take() else {
                        panic!("a query is answered with facts");
                    };
                    let spec = &self.steps[step];
                    let (actions, unobservable) = match facts.and_then(|facts| spec.actions(&facts))
                    {
                        Ok(actions) => (actions, None),
                        Err(failure) => (Vec::new(), Some(failure)),
                    };
                    let mut start = Start::new(
                        spec.key(),
                        Kind::Step(Step {
                            title: spec.title().to_string(),
                        }),
                    );
                    if spec.shielded() {
                        start = start.shielded();
                    }
                    let node = tree
                        .start(self.plan, start)
                        .expect("each step key is started once");
                    self.nodes[step] = Some(node);
                    if let Some(failure) = unobservable {
                        finish(tree, node, Ending::failed(diagnostic(&failure)));
                        self.verdict = Some(Verdict::Failed {
                            step: self.steps[step].key(),
                            failure,
                        });
                        self.roll_back();
                    } else if actions.is_empty() {
                        finish(tree, node, Ending::already_satisfied());
                        self.phase = Phase::Checking {
                            step: step + 1,
                            asked: false,
                        };
                    } else {
                        self.phase = Phase::Executing {
                            step,
                            node,
                            queue: actions.into(),
                            awaiting: false,
                        };
                    }
                }
                Phase::Executing {
                    step,
                    node,
                    mut queue,
                    awaiting: false,
                } => match self.stop {
                    Some(cause) if !self.steps[step].shielded() => {
                        finish(tree, node, Ending::cancelled(cause));
                        self.cancel(cause);
                    }
                    _ => match queue.pop_front() {
                        None => {
                            finish(tree, node, Ending::succeeded());
                            self.phase = Phase::Checking {
                                step: step + 1,
                                asked: false,
                            };
                        }
                        Some(action) => {
                            self.phase = Phase::Executing {
                                step,
                                node,
                                queue,
                                awaiting: true,
                            };
                            return self.act(tree, node, action);
                        }
                    },
                },
                Phase::Executing {
                    step,
                    node,
                    queue,
                    awaiting: true,
                } => {
                    let Some(Input::Done(outcome)) = input.take() else {
                        panic!("an action is answered with its outcome");
                    };
                    self.acted(tree, &outcome, self.stop);
                    match outcome {
                        Ok(performed) => {
                            self.journal[step].push(performed.undo);
                            self.phase = Phase::Executing {
                                step,
                                node,
                                queue,
                                awaiting: false,
                            };
                        }
                        Err(Failure::Cancelled) => {
                            let cause = *self.stop.get_or_insert(Cancellation::Interrupted);
                            finish(tree, node, Ending::cancelled(cause));
                            self.cancel(cause);
                        }
                        Err(failure) => {
                            finish(tree, node, Ending::failed(diagnostic(&failure)));
                            self.verdict = Some(Verdict::Failed {
                                step: self.steps[step].key(),
                                failure,
                            });
                            self.roll_back();
                        }
                    }
                }
                Phase::RollingBack {
                    mut remaining,
                    current: None,
                    ..
                } => match remaining.pop() {
                    None => self.phase = Phase::Closing,
                    Some(step) => {
                        let key = self.steps[step].key();
                        let node = tree
                            .start(
                                self.plan,
                                Start::new(
                                    format!("rollback:{key}"),
                                    Kind::Rollback(Rollback {
                                        undoes: self.nodes[step].expect("an undone step started"),
                                    }),
                                )
                                .shielded(),
                            )
                            .expect("each step is rolled back once");
                        self.phase = Phase::RollingBack {
                            remaining,
                            current: Some(Undoing {
                                step,
                                node,
                                queue: rollback_order(&self.journal[step]).into(),
                                doubtful: self
                                    .doubts
                                    .iter()
                                    .filter(|doubt| doubt.step == step)
                                    .flat_map(|doubt| {
                                        rollback_order(std::slice::from_ref(&doubt.undo))
                                    })
                                    .collect(),
                                doubting: false,
                                failed: false,
                            }),
                            awaiting: false,
                        };
                    }
                },
                Phase::RollingBack {
                    remaining,
                    current: Some(mut undoing),
                    awaiting: false,
                } => match undoing
                    .doubtful
                    .pop_front()
                    .map(|action| (action, true))
                    .or_else(|| undoing.queue.pop_front().map(|action| (action, false)))
                {
                    None => {
                        let ending = if undoing.failed {
                            Ending::failed(rollback_diagnostic(
                                &self.rollback_failures,
                                self.steps[undoing.step].key(),
                            ))
                        } else {
                            Ending::succeeded()
                        };
                        finish(tree, undoing.node, ending);
                        self.phase = Phase::RollingBack {
                            remaining,
                            current: None,
                            awaiting: false,
                        };
                    }
                    Some((action, doubting)) => {
                        undoing.doubting = doubting;
                        let node = undoing.node;
                        self.phase = Phase::RollingBack {
                            remaining,
                            current: Some(undoing),
                            awaiting: true,
                        };
                        return self.act(tree, node, action);
                    }
                },
                Phase::RollingBack {
                    remaining,
                    current: Some(mut undoing),
                    awaiting: true,
                } => {
                    let Some(Input::Done(outcome)) = input.take() else {
                        panic!("an undo is answered with its outcome");
                    };
                    self.acted(tree, &outcome, self.stop);
                    if let Err(failure) = outcome
                        && !undoing.doubting
                    {
                        undoing.failed = true;
                        self.rollback_failures
                            .push((self.steps[undoing.step].key(), failure));
                    }
                    self.phase = Phase::RollingBack {
                        remaining,
                        current: Some(undoing),
                        awaiting: false,
                    };
                }
                Phase::Committing { awaiting: false } => {
                    self.phase = Phase::Committing { awaiting: true };
                    return self.act(tree, self.plan, Action::Commit);
                }
                Phase::Committing { awaiting: true } => {
                    let Some(Input::Done(outcome)) = input.take() else {
                        panic!("the commit is answered with its outcome");
                    };
                    self.acted(tree, &outcome, self.stop);
                    if let Err(failure) = outcome {
                        tree.warn(self.plan, diagnostic(&failure))
                            .expect("the plan is running");
                    }
                    self.verdict = Some(Verdict::Succeeded);
                    self.phase = Phase::Closing;
                }
                Phase::Closing => {
                    let verdict = self.verdict.clone().unwrap_or(Verdict::Succeeded);
                    let mut ending = match &verdict {
                        Verdict::Succeeded => Ending::succeeded(),
                        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
                        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
                    };
                    if !self.rollback_failures.is_empty() {
                        let incomplete = incomplete(&self.rollback_failures);
                        ending = ending.with_diagnostic(incomplete);
                    }
                    finish(tree, self.plan, ending);
                    let report = Report {
                        verdict,
                        rollback_failures: self.rollback_failures.clone(),
                    };
                    self.phase = Phase::Closed(report.clone());
                    return Next::Finished(report);
                }
                Phase::Closed(report) => {
                    self.phase = Phase::Closed(report.clone());
                    return Next::Finished(report);
                }
            }
        }
    }

    fn cancel(&mut self, cause: Cancellation) {
        self.verdict = Some(Verdict::Cancelled(cause));
        self.roll_back();
    }

    fn roll_back(&mut self) {
        let remaining = (0..self.steps.len())
            .filter(|step| {
                !self.journal[*step].is_empty()
                    || self.doubts.iter().any(|doubt| doubt.step == *step)
            })
            .collect();
        self.phase = Phase::RollingBack {
            remaining,
            current: None,
            awaiting: false,
        };
    }
}

pub fn describe(action: &Action) -> (Operation, String) {
    let path = |path: &std::path::Path| path.display().to_string();
    match action {
        Action::CreateDir { path: at, .. } => (Operation::CreateDir, path(at)),
        Action::PutFile { path: at, .. } => (Operation::PutFile, path(at)),
        Action::SetMode { path: at, .. } => (Operation::SetMode, path(at)),
        Action::SetOwner { path: at, .. } => (Operation::SetOwner, path(at)),
        Action::SetAside { path: at, .. } => (Operation::SetAside, path(at)),
        Action::RemoveCreated { path: at, .. } => (Operation::RemoveCreated, path(at)),
        Action::RemoveCreatedTree { path: at, .. } => (Operation::RemoveCreatedTree, path(at)),
        Action::Restore { path: at, .. } => (Operation::Restore, path(at)),
        Action::ReclaimTree { path: at, .. } => (Operation::ReclaimTree, path(at)),
        Action::CopyTree { to, .. } => (Operation::CopyTree, path(to)),
        Action::AddGroup { name, .. } => (Operation::AddGroup, name.clone()),
        Action::SetGroupGid { name, .. } => (Operation::SetGroupGid, name.clone()),
        Action::DeleteGroup { name, .. } => (Operation::DeleteGroup, name.clone()),
        Action::AddUser(spec) => (Operation::AddUser, spec.name.clone()),
        Action::SetUserIds { name, .. } => (Operation::SetUserIds, name.clone()),
        Action::DeleteUser { name, .. } => (Operation::DeleteUser, name.clone()),
        Action::AddMember { group, user } => (Operation::AddMember, format!("{user} in {group}")),
        Action::RemoveMember { group, user } => {
            (Operation::RemoveMember, format!("{user} in {group}"))
        }
        Action::InstallUnit { unit, .. } => (Operation::InstallUnit, unit.clone()),
        Action::EnableUnit { unit } => (Operation::EnableUnit, unit.clone()),
        Action::DisableUnit { unit } => (Operation::DisableUnit, unit.clone()),
        Action::StartUnit { unit } => (Operation::StartUnit, unit.clone()),
        Action::StopUnit { unit } => (Operation::StopUnit, unit.clone()),
        Action::RestartUnit { unit } => (Operation::RestartUnit, unit.clone()),
        Action::DaemonReload => (Operation::DaemonReload, String::new()),
        Action::InstallRuntime { url, .. } => (Operation::InstallRuntime, url.clone()),
        Action::RemoveRuntime { .. } => (Operation::RemoveRuntime, String::new()),
        Action::ActivateProfile { user, .. } => (Operation::ActivateProfile, user.name.clone()),
        Action::SwitchGeneration { user, .. } => (Operation::SwitchGeneration, user.name.clone()),
        Action::DeleteGeneration { user, .. } => (Operation::DeleteGeneration, user.name.clone()),
        Action::RecordState { user } => (Operation::RecordState, user.name.clone()),
        Action::Commit => (Operation::Commit, String::new()),
    }
}

fn finish(tree: &mut Tree, node: NodeId, ending: Ending) {
    tree.finish(node, ending)
        .expect("the runner finishes its nodes children first");
}

fn incomplete(failures: &[(&'static str, Failure)]) -> Diagnostic {
    let mut steps: Vec<String> = failures.iter().map(|(step, _)| step.to_string()).collect();
    steps.dedup();
    Diagnostic {
        code: Code::RollbackIncomplete as i32,
        severity: Severity::Error as i32,
        node: 0,
        message: format!("{} undo action(s) failed", failures.len()),
        causes: failures
            .iter()
            .map(|(_, failure)| diagnostic(failure))
            .collect(),
        detail: Some(Detail::Steps(StepsDetail { steps })),
    }
}

fn rollback_diagnostic(failures: &[(&'static str, Failure)], step: &'static str) -> Diagnostic {
    let own: Vec<(&'static str, Failure)> = failures
        .iter()
        .filter(|(failed, _)| *failed == step)
        .cloned()
        .collect();
    incomplete(&own)
}

pub fn diagnostic(failure: &Failure) -> Diagnostic {
    let (code, message, detail) = match failure {
        Failure::Conflict {
            subject,
            expected,
            found,
        } => (
            Code::Conflict,
            format!("{subject}: expected {expected}, found {found}"),
            None,
        ),
        Failure::Io { path, kind } => (
            if *kind == std::io::ErrorKind::PermissionDenied {
                Code::PermissionDenied
            } else {
                Code::Io
            },
            format!("{}: {kind}", path.display()),
            Some(Detail::Io(IoDetail {
                path: path.display().to_string(),
                kind: format!("{kind:?}"),
            })),
        ),
        Failure::CommandFailed {
            program,
            status,
            output_tail,
        } => (
            Code::CommandFailed,
            format!("{program} failed"),
            Some(Detail::Command(CommandDetail {
                command: program.clone(),
                exit_status: *status,
                output_tail: output_tail.clone(),
            })),
        ),
        Failure::SpawnFailed { program, kind } => {
            (Code::SpawnFailed, format!("{program}: {kind}"), None)
        }
        Failure::Unit(unit) => (
            Code::UnitFailed,
            format!(
                "could not {} {}: job {}, {} ({}), result {}",
                unit.operation.verb(),
                unit.unit,
                unit.job_result,
                unit.active_state,
                unit.sub_state,
                unit.unit_result
            ),
            None,
        ),
        Failure::SystemdUnreachable => (
            Code::SystemdUnreachable,
            "systemd could not be reached".to_string(),
            None,
        ),
        Failure::Network { url } => (
            Code::Network,
            url.clone(),
            Some(Detail::Network(NetworkDetail { url: url.clone() })),
        ),
        Failure::Integrity {
            artifact,
            expected,
            found,
        } => (
            Code::Integrity,
            format!("{artifact}: expected {expected}, found {found}"),
            Some(Detail::Integrity(IntegrityDetail {
                artifact: artifact.clone(),
                expected: expected.clone(),
                actual: found.clone(),
            })),
        ),
        Failure::Cancelled => (Code::Internal, "cancelled".to_string(), None),
    };
    Diagnostic {
        code: code as i32,
        severity: Severity::Error as i32,
        node: 0,
        message,
        causes: Vec::new(),
        detail,
    }
}

#[cfg(test)]
mod tests;
