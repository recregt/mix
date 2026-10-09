use mix_events::v1::command::Request;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Nothing,
    Exclusive,
    SharedForUser,
    Observe,
}

pub fn locks_for(request: &Request, dry_run: bool) -> Need {
    if dry_run {
        return match request {
            Request::Explain(_) => Need::Nothing,
            Request::Bootstrap(_)
            | Request::Repair(_)
            | Request::Clean(_)
            | Request::Install(_)
            | Request::Remove(_)
            | Request::Doctor(_) => Need::Observe,
        };
    }
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
            locks_for(&Request::Bootstrap(Box::default()), false),
            Need::Exclusive
        );
        assert_eq!(
            locks_for(&Request::Repair(RepairRequest {}), false),
            Need::Exclusive
        );
        assert_eq!(
            locks_for(&Request::Clean(CleanRequest { all: true }), false),
            Need::Exclusive
        );
    }

    #[test]
    fn a_change_to_one_profile_shares_the_machine_and_queues_per_user() {
        assert_eq!(
            locks_for(&Request::Install(InstallRequest::default()), false),
            Need::SharedForUser
        );
        assert_eq!(
            locks_for(&Request::Clean(CleanRequest { all: false }), false),
            Need::SharedForUser
        );
    }

    #[test]
    fn a_request_that_reads_no_state_waits_for_no_one() {
        assert_eq!(
            locks_for(&Request::Explain(ExplainRequest::default()), false),
            Need::Nothing
        );
    }

    #[test]
    fn a_dry_run_never_takes_a_lock_for_itself_alone() {
        for request in [
            Request::Bootstrap(Box::default()),
            Request::Repair(RepairRequest {}),
            Request::Clean(CleanRequest { all: true }),
            Request::Install(InstallRequest::default()),
        ] {
            assert_eq!(locks_for(&request, true), Need::Observe, "{request:?}");
        }
    }
}
