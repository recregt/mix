use std::path::PathBuf;

pub use mix_core::BuildProgress;

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
    SpanOpened {
        id: u64,
        parent: Option<u64>,
        name: String,
        fields: Vec<(String, String)>,
    },
    SpanClosed {
        id: u64,
        failed: bool,
    },
    Log {
        level: Level,
        span: Option<u64>,
        message: String,
    },
    DownloadStarted {
        total: u64,
    },
    DownloadAdvanced {
        delta: u64,
    },
    ActivityLine(String),
    ActivityProgress(BuildProgress),
    ActivityCleared,
    Finished(Outcome),
}

#[derive(Debug)]
pub enum Outcome {
    BootstrapDone,
    RepairDone(Vec<RepairReport>),
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
    SourceBuildRequired {
        packages: Option<Vec<String>>,
    },
    Network(String),
    Integrity {
        artifact: String,
        detail: String,
    },
    UnsupportedTarget(String),
    Target(TargetFailure),
    Decompression(String),
    MalformedArchive(String),
    NotRoot(String),
    UnsupportedHost,
    UnsupportedKernel,
    SystemdNotReady {
        host: Host,
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
