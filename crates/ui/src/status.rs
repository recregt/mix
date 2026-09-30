pub(crate) const GUTTER: usize = 12;

macro_rules! verb {
    ($text:literal) => {{
        const TEXT: &str = $text;
        const _: () = assert!(TEXT.len() <= GUTTER, "a status verb must fit the gutter");
        TEXT
    }};
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Creating,
    Removing,
    Installing,
    Writing,
    Configuring,
    Activating,
    Recording,
    Restarting,
    Repairing,
    Recovering,
    Changing,
    Moving,
    Restoring,
    Reclaiming,
    Copying,
    Adding,
    Enabling,
    Disabling,
    Starting,
    Stopping,
    Reloading,
    Switching,
    Applying,
    Committing,
    RollingBack,
    Running,
    Fetching,
    Building,
    Observed,
    Journaled,
    Exited,
    Request,
    Installed,
    Removed,
    Ignored,
    Repaired,
    Checked,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Plain,
    Caution,
}

impl Status {
    pub const fn text(self) -> &'static str {
        match self {
            Status::Creating => verb!("Creating"),
            Status::Removing => verb!("Removing"),
            Status::Installing => verb!("Installing"),
            Status::Writing => verb!("Writing"),
            Status::Configuring => verb!("Configuring"),
            Status::Activating => verb!("Activating"),
            Status::Recording => verb!("Recording"),
            Status::Restarting => verb!("Restarting"),
            Status::Repairing => verb!("Repairing"),
            Status::Recovering => verb!("Recovering"),
            Status::Changing => verb!("Changing"),
            Status::Moving => verb!("Moving"),
            Status::Restoring => verb!("Restoring"),
            Status::Reclaiming => verb!("Reclaiming"),
            Status::Copying => verb!("Copying"),
            Status::Adding => verb!("Adding"),
            Status::Enabling => verb!("Enabling"),
            Status::Disabling => verb!("Disabling"),
            Status::Starting => verb!("Starting"),
            Status::Stopping => verb!("Stopping"),
            Status::Reloading => verb!("Reloading"),
            Status::Switching => verb!("Switching"),
            Status::Applying => verb!("Applying"),
            Status::Committing => verb!("Committing"),
            Status::RollingBack => verb!("Rolling back"),
            Status::Running => verb!("Running"),
            Status::Fetching => verb!("Fetching"),
            Status::Building => verb!("Building"),
            Status::Observed => verb!("Observed"),
            Status::Journaled => verb!("Journaled"),
            Status::Exited => verb!("Exited"),
            Status::Request => verb!("Request"),
            Status::Installed => verb!("Installed"),
            Status::Removed => verb!("Removed"),
            Status::Ignored => verb!("Ignored"),
            Status::Repaired => verb!("Repaired"),
            Status::Checked => verb!("Checked"),
            Status::Finished => verb!("Finished"),
        }
    }

    pub(crate) const fn tone(self) -> Tone {
        match self {
            Status::RollingBack | Status::Exited => Tone::Caution,
            Status::Creating
            | Status::Removing
            | Status::Installing
            | Status::Writing
            | Status::Configuring
            | Status::Activating
            | Status::Recording
            | Status::Restarting
            | Status::Repairing
            | Status::Recovering
            | Status::Changing
            | Status::Moving
            | Status::Restoring
            | Status::Reclaiming
            | Status::Copying
            | Status::Adding
            | Status::Enabling
            | Status::Disabling
            | Status::Starting
            | Status::Stopping
            | Status::Reloading
            | Status::Switching
            | Status::Applying
            | Status::Committing
            | Status::Running
            | Status::Fetching
            | Status::Building
            | Status::Observed
            | Status::Journaled
            | Status::Request
            | Status::Installed
            | Status::Removed
            | Status::Ignored
            | Status::Repaired
            | Status::Checked
            | Status::Finished => Tone::Plain,
        }
    }
}
