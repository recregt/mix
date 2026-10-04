use std::collections::HashMap;

use mix_events::result::FORMAT_VERSION;
use mix_events::result::v1::{
    Change, ChangeAction, Problem, Result as Document, StepResult, problem, result,
};
use mix_events::v1::{
    Diagnostic, Envelope, NodeFinished, Operation, Severity, Status, diagnostic, envelope,
    node_finished, node_progress, node_started,
};
use mix_events::{NodeId, ROOT};

enum Node {
    Step(String),
    Rollback(NodeId),
    Other,
}

#[derive(Default)]
pub(crate) struct Builder {
    document: Document,
    nodes: HashMap<NodeId, (NodeId, Node)>,
    changes: HashMap<NodeId, usize>,
    steps: HashMap<NodeId, usize>,
    rolled_back: Vec<NodeId>,
}

impl Builder {
    pub(crate) fn envelope(&mut self, envelope: &Envelope) -> Option<Document> {
        if self.document.request.is_empty() {
            self.document.request.clone_from(&envelope.request);
        }
        match envelope.event.as_ref()? {
            envelope::Event::NodeStarted(started) => {
                let node = match &started.kind {
                    Some(node_started::Kind::Command(command)) => {
                        self.document.command =
                            mix_events::key_of(command.request.as_ref()).to_string();
                        self.document.dry_run = command.dry_run;
                        Node::Other
                    }
                    Some(node_started::Kind::Step(step)) => {
                        self.steps.insert(started.id, self.document.steps.len());
                        self.document.steps.push(StepResult {
                            key: started.key.clone(),
                            verb: step.verb,
                            subject: step.subject.clone(),
                            status: Status::Unspecified as i32,
                            undone: false,
                        });
                        Node::Step(started.key.clone())
                    }
                    Some(node_started::Kind::LockWait(wait)) => {
                        self.document.waits.push(wait.clone());
                        Node::Other
                    }
                    Some(node_started::Kind::Rollback(rollback)) => Node::Rollback(rollback.undoes),
                    Some(node_started::Kind::Action(action)) => {
                        self.action(started.id, started.parent, action);
                        Node::Other
                    }
                    Some(
                        node_started::Kind::Plan(_)
                        | node_started::Kind::Process(_)
                        | node_started::Kind::Download(_)
                        | node_started::Kind::Inspection(_)
                        | node_started::Kind::NixActivity(_),
                    )
                    | None => Node::Other,
                };
                self.nodes.insert(started.id, (started.parent, node));
                None
            }
            envelope::Event::NodeFinished(finished) if finished.id == ROOT => {
                Some(self.finish(finished))
            }
            envelope::Event::NodeFinished(finished) => {
                if let Some(index) = self.changes.get(&finished.id) {
                    self.document.changes[*index].status = finished.status;
                }
                if let Some(index) = self.steps.get(&finished.id) {
                    self.document.steps[*index].status = finished.status;
                }
                if let Some((_, Node::Rollback(step))) = self.nodes.get(&finished.id)
                    && finished.status() == Status::Succeeded
                {
                    self.rolled_back.push(*step);
                }
                None
            }
            envelope::Event::Diagnostic(found) if found.severity() == Severity::Warning => {
                self.document.warnings.push(problem_of(found));
                None
            }
            envelope::Event::NodeProgress(progress) => {
                if let Some(node_progress::Progress::Waiting(wait)) = &progress.progress {
                    self.document.waits.push(wait.clone());
                }
                None
            }
            envelope::Event::Diagnostic(_) | envelope::Event::NotRun(_) => None,
        }
    }

    fn action(&mut self, id: NodeId, parent: NodeId, action: &mix_events::v1::Action) {
        let mut at = parent;
        loop {
            match self.nodes.get(&at) {
                Some((_, Node::Rollback(_))) => return,
                Some((_, Node::Step(key))) => {
                    let Some(kind) = change_action(action.operation()) else {
                        return;
                    };
                    self.changes.insert(id, self.document.changes.len());
                    let unknown = if self.document.dry_run {
                        known_after(action.operation())
                    } else {
                        Vec::new()
                    };
                    self.document.changes.push(Change {
                        step: key.clone(),
                        action: kind as i32,
                        operation: action.operation,
                        subject: action.subject.clone(),
                        status: Status::Unspecified as i32,
                        undone: false,
                        unknown,
                    });
                    return;
                }
                Some((up, Node::Other)) if *up != at => at = *up,
                _ => return,
            }
        }
    }

    fn finish(&mut self, root: &NodeFinished) -> Document {
        let mut document = std::mem::take(&mut self.document);
        document.format_version = FORMAT_VERSION.to_string();
        document.status = root.status;
        document.exit = root.exit_code;
        document.cancellation = root.cancellation;
        document.result = root.result.clone().and_then(result_of);
        if let Some(found) = &root.diagnostic {
            document.problems.push(problem_of(found));
        }
        if let Some(node_finished::Result::Repair(repair)) = &root.result {
            for report in &repair.reports {
                if let Some(failure) = &report.failure {
                    let mut problem = problem_of(failure);
                    if problem.subject.is_empty() {
                        problem.subject.clone_from(&report.target);
                    }
                    document.problems.push(problem);
                }
            }
        }
        let mut undone = Vec::new();
        for step in &self.rolled_back {
            if let Some(index) = self.steps.get(step) {
                document.steps[*index].undone = true;
                undone.push(document.steps[*index].key.clone());
            }
        }
        for change in &mut document.changes {
            change.undone = change.status() == Status::Succeeded && undone.contains(&change.step);
        }
        document
    }
}

fn result_of(found: node_finished::Result) -> Option<result::Result> {
    Some(match found {
        node_finished::Result::Bootstrap(found) => result::Result::Bootstrap(found),
        node_finished::Result::Install(found) => result::Result::Install(found),
        node_finished::Result::Remove(found) => result::Result::Remove(found),
        node_finished::Result::Repair(found) => result::Result::Repair(found),
        node_finished::Result::Doctor(found) => result::Result::Doctor(found),
        node_finished::Result::Clean(found) => result::Result::Clean(found),
        node_finished::Result::Explain(found) => result::Result::Explain(found),
        node_finished::Result::Inspection(_) | node_finished::Result::Process(_) => return None,
    })
}

fn known_after(operation: Operation) -> Vec<String> {
    let unknown: &[&str] = match operation {
        Operation::ActivateProfile => &["generation", "build"],
        Operation::InstallRuntime => &["contents"],
        Operation::CollectGarbage => &["freed"],
        Operation::CreateRepository | Operation::RecordState => &["commit"],
        Operation::CreateDir
        | Operation::CreateDirs
        | Operation::PutFile
        | Operation::SetMode
        | Operation::SetOwner
        | Operation::SetAside
        | Operation::RemoveCreated
        | Operation::RemoveCreatedTree
        | Operation::Restore
        | Operation::ReclaimTree
        | Operation::CopyTree
        | Operation::AddGroup
        | Operation::SetGroupGid
        | Operation::DeleteGroup
        | Operation::AddUser
        | Operation::SetUserIds
        | Operation::DeleteUser
        | Operation::AddMember
        | Operation::RemoveMember
        | Operation::InstallUnit
        | Operation::EnableUnit
        | Operation::DisableUnit
        | Operation::StartUnit
        | Operation::StopUnit
        | Operation::RestartUnit
        | Operation::DrainService
        | Operation::DaemonReload
        | Operation::RemoveRuntime
        | Operation::SwitchGeneration
        | Operation::DeleteGeneration
        | Operation::ApplyGeneration
        | Operation::Commit
        | Operation::Unspecified => &[],
    };
    unknown.iter().map(|field| (*field).to_string()).collect()
}

fn change_action(operation: Operation) -> Option<ChangeAction> {
    Some(match operation {
        Operation::CreateDir
        | Operation::CreateDirs
        | Operation::CopyTree
        | Operation::AddGroup
        | Operation::AddUser
        | Operation::AddMember
        | Operation::InstallRuntime
        | Operation::CreateRepository => ChangeAction::Create,
        Operation::RemoveCreated
        | Operation::RemoveCreatedTree
        | Operation::SetAside
        | Operation::DeleteGroup
        | Operation::DeleteUser
        | Operation::RemoveMember
        | Operation::RemoveRuntime
        | Operation::DeleteGeneration
        | Operation::CollectGarbage => ChangeAction::Delete,
        Operation::PutFile
        | Operation::SetMode
        | Operation::SetOwner
        | Operation::Restore
        | Operation::ReclaimTree
        | Operation::SetGroupGid
        | Operation::SetUserIds
        | Operation::InstallUnit
        | Operation::EnableUnit
        | Operation::DisableUnit
        | Operation::StartUnit
        | Operation::StopUnit
        | Operation::RestartUnit
        | Operation::DrainService
        | Operation::DaemonReload
        | Operation::ActivateProfile
        | Operation::SwitchGeneration
        | Operation::ApplyGeneration
        | Operation::RecordState => ChangeAction::Update,
        Operation::Commit | Operation::Unspecified => return None,
    })
}

pub(crate) fn problem_of(found: &Diagnostic) -> Problem {
    let code = found.code();
    let explanation = mix_explain::codes::explanation(code);
    let (subject, metadata) = match found.detail.clone() {
        Some(diagnostic::Detail::Io(detail)) => {
            (detail.path.clone(), Some(problem::Metadata::Io(detail)))
        }
        Some(diagnostic::Detail::Command(detail)) => (
            detail.command.clone(),
            Some(problem::Metadata::Command(detail)),
        ),
        Some(diagnostic::Detail::Network(detail)) => {
            (detail.url.clone(), Some(problem::Metadata::Network(detail)))
        }
        Some(diagnostic::Detail::Integrity(detail)) => (
            detail.artifact.clone(),
            Some(problem::Metadata::Integrity(detail)),
        ),
        Some(diagnostic::Detail::Target(detail)) => {
            (String::new(), Some(problem::Metadata::Target(detail)))
        }
        Some(diagnostic::Detail::Host(detail)) => {
            (String::new(), Some(problem::Metadata::Host(detail)))
        }
        Some(diagnostic::Detail::Path(detail)) => {
            (detail.path.clone(), Some(problem::Metadata::Path(detail)))
        }
        Some(diagnostic::Detail::Packages(detail)) => {
            (String::new(), Some(problem::Metadata::Packages(detail)))
        }
        Some(diagnostic::Detail::Format(detail)) => {
            (String::new(), Some(problem::Metadata::Format(detail)))
        }
        Some(diagnostic::Detail::Unrepairable(detail)) => (
            detail.artifact.clone(),
            Some(problem::Metadata::Unrepairable(detail)),
        ),
        Some(diagnostic::Detail::Lock(detail)) => {
            (detail.path.clone(), Some(problem::Metadata::Lock(detail)))
        }
        Some(diagnostic::Detail::Steps(detail)) => {
            (String::new(), Some(problem::Metadata::Steps(detail)))
        }
        Some(diagnostic::Detail::Conflict(detail)) => (
            detail.subject.clone(),
            Some(problem::Metadata::Conflict(*detail)),
        ),
        Some(diagnostic::Detail::Unit(detail)) => {
            (detail.unit.clone(), Some(problem::Metadata::Unit(*detail)))
        }
        None => (String::new(), None),
    };
    Problem {
        r#type: format!("urn:mix:problem:{}", mix_events::code::kebab(code)),
        code: code as i32,
        title: explanation
            .description
            .strip_suffix('.')
            .unwrap_or(explanation.description)
            .to_string(),
        detail: found.message.clone(),
        subject,
        help: explanation
            .fix
            .iter()
            .map(|fix| (*fix).to_string())
            .collect(),
        causes: found.causes.iter().map(problem_of).collect(),
        metadata,
    }
}

pub fn usage(request: String, detail: &str) -> Document {
    Document {
        format_version: FORMAT_VERSION.to_string(),
        request,
        status: Status::Failed as i32,
        exit: mix_events::exit::USAGE,
        problems: vec![problem_of(&Diagnostic {
            code: mix_events::v1::Code::Usage as i32,
            severity: Severity::Error as i32,
            message: detail.to_string(),
            ..Diagnostic::default()
        })],
        ..Document::default()
    }
}

pub fn of<'e>(envelopes: impl IntoIterator<Item = &'e Envelope>) -> Option<Document> {
    let mut builder = Builder::default();
    envelopes
        .into_iter()
        .find_map(|envelope| builder.envelope(envelope))
}

pub(crate) fn print(document: &Document) {
    if let Ok(text) = serde_json::to_string_pretty(document) {
        mix_ui::data(&text);
    }
}

#[cfg(test)]
mod tests;
