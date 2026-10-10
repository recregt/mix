pub mod journal;
pub mod locks;

use std::borrow::Cow;
use std::collections::VecDeque;

use generativity::Id;
pub use generativity::{Guard, make_guard};
use mix_events::v1::{
    Action as ActionNode, Cancellation, Code, Diagnostic, Plan, Rollback, Severity, Step,
    StepsDetail, Verb, diagnostic::Detail, node_started::Kind,
};
use mix_events::{Ending, NodeId, Start, Tree};

use crate::effect::{Action, Fact, Failure, Outcome, Query};
use crate::report::diagnose::diagnostic;
use crate::run::journal::rollback_order;

pub struct Title {
    pub verb: Verb,
    pub subject: Cow<'static, str>,
}

impl Title {
    pub fn new(verb: Verb, subject: impl Into<Cow<'static, str>>) -> Self {
        Self {
            verb,
            subject: subject.into(),
        }
    }
}

pub trait StepSpec: Send + Sync {
    fn key(&self) -> Cow<'static, str>;
    fn title(&self) -> Title;
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

#[derive(Debug, PartialEq, Eq)]
pub enum Next<'id> {
    Observe(Vec<Query>),
    Perform(Action),
    Finished(Closed<'id>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Closed<'id>(Id<'id>);

pub struct Session<'id, 'r> {
    runner: &'r mut Runner,
    id: Id<'id>,
}

impl<'id, 'r> Session<'id, 'r> {
    pub fn step(&mut self, tree: &mut Tree, input: Option<Input>) -> Next<'id> {
        self.runner.step(self.id, tree, input)
    }

    pub fn report(self, _: Closed<'id>) -> &'r mut Report {
        &mut self.runner.report
    }

    pub fn current_node(&self) -> Option<NodeId> {
        self.runner.current_node()
    }

    pub fn rolling_back(&self) -> bool {
        self.runner.rolling_back()
    }

    pub fn shielded(&self) -> bool {
        self.runner.shielded()
    }

    pub fn in_doubt(&mut self, undo: Vec<Action>) {
        self.runner.in_doubt(undo);
    }

    pub fn stop(&mut self, cause: Cancellation) {
        self.runner.stop(cause);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Succeeded,
    Failed {
        step: Cow<'static, str>,
        failure: Failure,
    },
    Cancelled(Cancellation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Satisfied,
    Changed,
    Failed(Failure),
    Cancelled(Cancellation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub verdict: Verdict,
    pub rollback_failures: Vec<(Cow<'static, str>, Failure)>,
    pub steps: Vec<(Cow<'static, str>, StepOutcome)>,
}

enum After {
    Close,
    Continue(usize),
    Commit,
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

#[repr(u8)]
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
        current: Option<Box<Undoing>>,
        awaiting: bool,
    },
    Committing {
        awaiting: bool,
    },
    Closing,
    Closed,
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
    report: Report,
    acting: Option<NodeId>,
    performed: u64,
    independent: bool,
    after: After,
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
            report: Report {
                verdict: Verdict::Succeeded,
                rollback_failures: Vec::new(),
                steps: Vec::new(),
            },
            acting: None,
            performed: 0,
            independent: false,
            after: After::Close,
        }
    }

    pub fn brand<'id>(&mut self, guard: Guard<'id>) -> Session<'id, '_> {
        Session {
            runner: self,
            id: guard.into(),
        }
    }

    pub fn independent(mut self) -> Self {
        self.independent = true;
        self.report.steps.reserve_exact(self.steps.len());
        self
    }

    fn record(&mut self, step: usize, outcome: impl FnOnce() -> StepOutcome) {
        if self.independent {
            self.report.steps.push((self.steps[step].key(), outcome()));
        }
    }

    fn fail(&mut self, step: usize, failure: Failure) {
        self.record(step, || StepOutcome::Failed(failure.clone()));
        let failed = Verdict::Failed {
            step: self.steps[step].key(),
            failure,
        };
        if self.independent {
            self.verdict.get_or_insert(failed);
            self.roll_back_only(step, After::Continue(step + 1));
        } else {
            self.verdict = Some(failed);
            self.roll_back();
        }
    }

    fn act<'id>(&mut self, tree: &mut Tree, parent: NodeId, action: Action) -> Next<'id> {
        self.performed += 1;
        let (operation, subject) = action.describe();
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

    fn current_node(&self) -> Option<NodeId> {
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

    fn rolling_back(&self) -> bool {
        matches!(self.phase, Phase::RollingBack { .. })
    }

    fn shielded(&self) -> bool {
        match &self.phase {
            Phase::RollingBack { .. } | Phase::Committing { .. } => true,
            Phase::Executing { step, .. } => self.steps[*step].shielded(),
            _ => false,
        }
    }

    fn in_doubt(&mut self, undo: Vec<Action>) {
        if let Phase::Executing { step, .. } = self.phase
            && !undo.is_empty()
        {
            self.doubts.push(Doubt { step, undo });
        }
    }

    fn stop(&mut self, cause: Cancellation) {
        self.stop.get_or_insert(cause);
    }

    fn step<'id>(&mut self, id: Id<'id>, tree: &mut Tree, input: Option<Input>) -> Next<'id> {
        let mut input = input;
        loop {
            match std::mem::replace(&mut self.phase, Phase::Closing) {
                Phase::Opening => {
                    self.plan = tree
                        .start(
                            self.parent,
                            Start::new("plan", Kind::Plan(Plan::default()))
                                .planned(self.steps.iter().map(|step| step.key())),
                        )
                        .expect("the plan starts under a running parent");
                    self.phase = Phase::Checking {
                        step: 0,
                        asked: false,
                    };
                }
                Phase::Checking { step, asked: false } => {
                    if step == self.steps.len() {
                        self.verdict.get_or_insert(Verdict::Succeeded);
                        self.finish_or_commit();
                    } else if let Some(cause) = self.stop {
                        self.cancel(cause, None);
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
                    let title = spec.title();
                    let mut start = Start::new(
                        spec.key(),
                        Kind::Step(Step {
                            verb: title.verb as i32,
                            subject: title.subject.into_owned(),
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
                        self.fail(step, failure);
                    } else if actions.is_empty() {
                        finish(tree, node, Ending::already_satisfied());
                        self.record(step, || StepOutcome::Satisfied);
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
                        self.record(step, || StepOutcome::Cancelled(cause));
                        self.cancel(cause, Some(step));
                    }
                    _ => match queue.pop_front() {
                        None => {
                            finish(tree, node, Ending::succeeded());
                            self.record(step, || StepOutcome::Changed);
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
                            self.record(step, || StepOutcome::Cancelled(cause));
                            self.cancel(cause, Some(step));
                        }
                        Err(failure) => {
                            finish(tree, node, Ending::failed(diagnostic(&failure)));
                            self.fail(step, failure);
                        }
                    }
                }
                Phase::RollingBack {
                    mut remaining,
                    current: None,
                    ..
                } => match remaining.pop() {
                    None => match std::mem::replace(&mut self.after, After::Close) {
                        After::Close => self.phase = Phase::Closing,
                        After::Continue(next) => {
                            self.phase = Phase::Checking {
                                step: next,
                                asked: false,
                            };
                        }
                        After::Commit => self.finish_or_commit(),
                    },
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
                        let queue = rollback_order(&self.journal[step]).into();
                        let doubtful = self
                            .doubts
                            .iter()
                            .filter(|doubt| doubt.step == step)
                            .flat_map(|doubt| rollback_order(std::slice::from_ref(&doubt.undo)))
                            .collect();
                        self.journal[step].clear();
                        self.doubts.retain(|doubt| doubt.step != step);
                        self.phase = Phase::RollingBack {
                            remaining,
                            current: Some(Box::new(Undoing {
                                step,
                                node,
                                queue,
                                doubtful,
                                doubting: false,
                                failed: false,
                            })),
                            awaiting: false,
                        };
                    }
                },
                Phase::RollingBack {
                    remaining,
                    current: Some(mut undoing),
                    awaiting: false,
                } => {
                    match undoing
                        .doubtful
                        .pop_front()
                        .map(|action| (action, true))
                        .or_else(|| undoing.queue.pop_front().map(|action| (action, false)))
                    {
                        None => {
                            let ending = if undoing.failed {
                                Ending::failed(rollback_diagnostic(
                                    &self.report.rollback_failures,
                                    &self.steps[undoing.step].key(),
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
                    }
                }
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
                        self.report
                            .rollback_failures
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
                    self.verdict.get_or_insert(Verdict::Succeeded);
                    self.phase = Phase::Closing;
                }
                Phase::Closing => {
                    let verdict = self.verdict.take().unwrap_or(Verdict::Succeeded);
                    let ending = match &verdict {
                        Verdict::Succeeded => Ending::succeeded(),
                        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
                        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
                    };
                    if !self.report.rollback_failures.is_empty() {
                        tree.warn(self.plan, incomplete(&self.report.rollback_failures))
                            .expect("the plan is open until it finishes");
                    }
                    finish(tree, self.plan, ending);
                    self.report.verdict = verdict;
                    self.phase = Phase::Closed;
                    return Next::Finished(Closed(id));
                }
                Phase::Closed => {
                    self.phase = Phase::Closed;
                    return Next::Finished(Closed(id));
                }
            }
        }
    }

    fn cancel(&mut self, cause: Cancellation, current: Option<usize>) {
        self.verdict = Some(Verdict::Cancelled(cause));
        if !self.independent {
            self.roll_back();
            return;
        }
        match current {
            Some(step)
                if !self.journal[step].is_empty()
                    || self.doubts.iter().any(|doubt| doubt.step == step) =>
            {
                self.roll_back_only(step, After::Commit);
            }
            _ => self.finish_or_commit(),
        }
    }

    fn finish_or_commit(&mut self) {
        self.phase = if self.journal.iter().all(Vec::is_empty) {
            Phase::Closing
        } else {
            Phase::Committing { awaiting: false }
        };
    }

    fn roll_back_only(&mut self, step: usize, after: After) {
        let owned =
            !self.journal[step].is_empty() || self.doubts.iter().any(|doubt| doubt.step == step);
        self.after = after;
        self.phase = Phase::RollingBack {
            remaining: if owned { vec![step] } else { Vec::new() },
            current: None,
            awaiting: false,
        };
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

fn finish(tree: &mut Tree, node: NodeId, ending: Ending) {
    tree.finish(node, ending)
        .expect("the runner finishes its nodes children first");
}

fn incomplete(failures: &[(Cow<'static, str>, Failure)]) -> Diagnostic {
    let mut steps: Vec<String> = failures.iter().map(|(step, _)| step.to_string()).collect();
    steps.dedup();
    Diagnostic {
        code: Code::RollbackIncomplete as i32,
        severity: Severity::Warning as i32,
        node: 0,
        message: format!("{} undo action(s) failed", failures.len()),
        causes: failures
            .iter()
            .map(|(_, failure)| diagnostic(failure))
            .collect(),
        detail: Some(Detail::Steps(StepsDetail { steps })),
    }
}

fn rollback_diagnostic(failures: &[(Cow<'static, str>, Failure)], step: &str) -> Diagnostic {
    let own: Vec<(Cow<'static, str>, Failure)> = failures
        .iter()
        .filter(|(failed, _)| *failed == step)
        .cloned()
        .collect();
    incomplete(&own)
}

#[cfg(test)]
mod tests;
