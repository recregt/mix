#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use mix_events::v1::{Operation, Verb};
use mix_ui::Status;

pub(crate) fn step(verb: Verb) -> Status {
    match verb {
        Verb::Creating => Status::Creating,
        Verb::Removing => Status::Removing,
        Verb::Installing => Status::Installing,
        Verb::Writing => Status::Writing,
        Verb::Configuring => Status::Configuring,
        Verb::Activating => Status::Activating,
        Verb::Recording => Status::Recording,
        Verb::Restarting => Status::Restarting,
        Verb::Repairing => Status::Repairing,
        Verb::Recovering => Status::Recovering,
        Verb::Unspecified => Status::Changing,
    }
}

pub(crate) fn undone(verb: Verb, subject: &str) -> String {
    match verb {
        Verb::Removing => format!("removal of {subject}"),
        Verb::Installing => format!("install of {subject}"),
        Verb::Activating => format!("activation of {subject}"),
        Verb::Restarting => format!("restart of {subject}"),
        Verb::Repairing => format!("repair of {subject}"),
        Verb::Recovering => format!("recovery of {subject}"),
        Verb::Creating
        | Verb::Writing
        | Verb::Configuring
        | Verb::Recording
        | Verb::Unspecified => subject.to_string(),
    }
}

pub(crate) fn action(operation: Operation) -> Status {
    match operation {
        Operation::CreateDir | Operation::CreateDirs => Status::Creating,
        Operation::PutFile => Status::Writing,
        Operation::SetMode
        | Operation::SetOwner
        | Operation::SetGroupGid
        | Operation::SetUserIds
        | Operation::Unspecified => Status::Changing,
        Operation::SetAside => Status::Moving,
        Operation::Restore => Status::Restoring,
        Operation::ReclaimTree => Status::Reclaiming,
        Operation::CopyTree => Status::Copying,
        Operation::AddGroup | Operation::AddUser | Operation::AddMember => Status::Adding,
        Operation::RemoveCreated
        | Operation::RemoveCreatedTree
        | Operation::DeleteGroup
        | Operation::DeleteUser
        | Operation::RemoveMember
        | Operation::RemoveRuntime
        | Operation::DeleteGeneration => Status::Removing,
        Operation::InstallUnit | Operation::InstallRuntime => Status::Installing,
        Operation::EnableUnit => Status::Enabling,
        Operation::DisableUnit => Status::Disabling,
        Operation::StartUnit => Status::Starting,
        Operation::StopUnit => Status::Stopping,
        Operation::RestartUnit => Status::Restarting,
        Operation::DaemonReload => Status::Reloading,
        Operation::ActivateProfile => Status::Activating,
        Operation::SwitchGeneration => Status::Switching,
        Operation::ApplyGeneration => Status::Applying,
        Operation::RecordState => Status::Recording,
        Operation::Commit => Status::Committing,
    }
}
