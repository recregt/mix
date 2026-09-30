use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirror {
    pub url: String,
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapRequest {
    pub mirror: Option<Mirror>,
    pub force: bool,
    pub log_level: Level,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairRequest {
    pub log_level: Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug)]
pub enum Event {
    Envelope(Vec<u8>),
    Log {
        level: Level,
        node: u64,
        message: String,
    },
    Finished(Outcome),
}

#[derive(Debug)]
pub enum Outcome {
    BootstrapDone,
    RepairDone {
        reports: Vec<RepairReport>,
        interrupted: bool,
    },
    Failure(Failure),
}

#[derive(Debug)]
pub struct RepairReport {
    pub name: String,
    pub failure: Option<TargetFailure>,
}

#[derive(Debug)]
pub enum Failure {
    Core(mix_core::Error),
    Network(String),
    Integrity {
        artifact: String,
        detail: String,
    },
    UnsupportedTarget(String),
    InvalidMirror(String),
    Conflict {
        subject: String,
        expected: String,
        found: String,
    },
    Target(TargetFailure),
    Decompression(String),
    MalformedArchive(String),
    NotRoot(String),
    UnsupportedHost,
    UnsupportedKernel,
    SystemdNotReady {
        host: Host,
    },
    SystemdUnreachable,
    Unit {
        operation: String,
        unit: String,
        detail: String,
        invocation: Option<String>,
    },
    AlreadyManaged,
    CrossDeviceStore {
        path: PathBuf,
    },
    Rollback {
        cause: Box<Failure>,
        summary: String,
    },
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Native,
    Wsl,
}

#[derive(Debug)]
pub enum TargetFailure {
    Core(mix_core::Error),
    Unrepairable { artifact: String, reason: Unfixable },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unfixable {
    NotADirectory,
    MissingUser,
    MissingRuntime,
}
