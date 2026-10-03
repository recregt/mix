use mix_events::v1::command::Request;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Nothing,
    Exclusive,
    Shared,
    SharedForUser,
}

pub fn locks_for(request: &Request) -> Need {
    match request {
        Request::Bootstrap(_) | Request::Repair(_) => Need::Exclusive,
        Request::Clean(clean) if clean.all => Need::Exclusive,
        Request::Install(_) | Request::Remove(_) | Request::Doctor(_) | Request::Clean(_) => {
            Need::SharedForUser
        }
        Request::Explain(_) => Need::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use mix_events::v1::{CleanRequest, ExplainRequest, InstallRequest, RepairRequest};

    use super::*;

    #[test]
    fn whatever_changes_the_whole_machine_runs_alone() {
        assert_eq!(
            locks_for(&Request::Bootstrap(Box::default())),
            Need::Exclusive
        );
        assert_eq!(
            locks_for(&Request::Repair(RepairRequest {})),
            Need::Exclusive
        );
        assert_eq!(
            locks_for(&Request::Clean(CleanRequest { all: true })),
            Need::Exclusive
        );
    }

    #[test]
    fn a_change_to_one_profile_shares_the_machine_and_queues_per_user() {
        assert_eq!(
            locks_for(&Request::Install(InstallRequest::default())),
            Need::SharedForUser
        );
        assert_eq!(
            locks_for(&Request::Clean(CleanRequest { all: false })),
            Need::SharedForUser
        );
    }

    #[test]
    fn a_request_that_reads_no_state_waits_for_no_one() {
        assert_eq!(
            locks_for(&Request::Explain(ExplainRequest::default())),
            Need::Nothing
        );
    }
}
