#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use crate::tree::ROOT;
use crate::v1::{Status, envelope::Event, node_progress::Progress, node_started::Kind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Detail {
    Outcome,
    Step,
    Action,
    Trace,
}

pub fn detail(event: &Event) -> Detail {
    match event {
        Event::NodeStarted(started) => match &started.kind {
            Some(Kind::Step(_) | Kind::Rollback(_) | Kind::LockWait(_)) => Detail::Step,
            Some(
                Kind::Command(_)
                | Kind::Plan(_)
                | Kind::Process(_)
                | Kind::Download(_)
                | Kind::Inspection(_)
                | Kind::NixActivity(_)
                | Kind::Action(_),
            ) => Detail::Action,
            None => Detail::Trace,
        },
        Event::NodeFinished(finished) if finished.id == ROOT => Detail::Outcome,
        Event::NodeFinished(finished) if finished.status() == Status::Failed => Detail::Step,
        Event::NodeFinished(_) | Event::NotRun(_) => Detail::Action,
        Event::NodeProgress(progress) => match &progress.progress {
            Some(Progress::Bytes(_) | Progress::Builds(_) | Progress::Stopping(_)) => Detail::Step,
            Some(
                Progress::Command(_)
                | Progress::CommandFinished(_)
                | Progress::Fetch(_)
                | Progress::Build(_),
            ) => Detail::Action,
            Some(Progress::Line(_) | Progress::Observed(_) | Progress::Journaled(_)) | None => {
                Detail::Trace
            }
        },
        Event::Diagnostic(_) => Detail::Step,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::{NodeFinished, NodeProgress, OutputLine, Step, node_progress};

    fn finished(id: u64, status: Status) -> Event {
        Event::NodeFinished(NodeFinished {
            id,
            status: status as i32,
            ..NodeFinished::default()
        })
    }

    #[test]
    fn the_outcome_is_the_roots_end_and_a_failure_is_as_visible_as_a_step() {
        assert_eq!(detail(&finished(ROOT, Status::Succeeded)), Detail::Outcome);
        assert_eq!(detail(&finished(4, Status::Failed)), Detail::Step);
        assert_eq!(detail(&finished(4, Status::Succeeded)), Detail::Action);
    }

    #[test]
    fn a_programs_own_output_is_the_most_detailed_thing_there_is() {
        let line = Event::NodeProgress(NodeProgress {
            id: 3,
            progress: Some(node_progress::Progress::Line(OutputLine::default())),
        });
        let step = Event::NodeStarted(crate::v1::NodeStarted {
            kind: Some(Kind::Step(Step::default())),
            ..crate::v1::NodeStarted::default()
        });

        assert_eq!(detail(&line), Detail::Trace);
        assert_eq!(detail(&step), Detail::Step);
        assert!(Detail::Outcome < Detail::Step && Detail::Action < Detail::Trace);
    }
}
