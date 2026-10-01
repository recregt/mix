use mix_events::v1::Envelope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Interrupt,
    Terminate,
    Pause,
    Resume,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Envelope(Envelope),
    Applied(Control),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the worker sent a message this version of mix cannot read: {0}")]
pub struct Malformed(pub String);
