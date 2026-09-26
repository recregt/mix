use std::io::ErrorKind;

use crate::proto;
use crate::types::{
    BootstrapRequest, BuildProgress, Event, Failure, Host, Level, Mirror, Outcome, RepairReport,
    RepairRequest, TargetFailure, Unfixable,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the worker sent a message this version of mix cannot read: {0}")]
pub struct Malformed(pub String);

fn missing(what: &str) -> Malformed {
    Malformed(format!("{what} is missing"))
}

fn level_to_wire(level: Level) -> i32 {
    match level {
        Level::Error => proto::Level::Error,
        Level::Warn => proto::Level::Warn,
        Level::Info => proto::Level::Info,
        Level::Debug => proto::Level::Debug,
        Level::Trace => proto::Level::Trace,
    }
    .into()
}

fn level_from_wire(value: i32) -> Result<Level, Malformed> {
    match proto::Level::try_from(value) {
        Ok(proto::Level::Error) => Ok(Level::Error),
        Ok(proto::Level::Warn) => Ok(Level::Warn),
        Ok(proto::Level::Info) => Ok(Level::Info),
        Ok(proto::Level::Debug) => Ok(Level::Debug),
        Ok(proto::Level::Trace) => Ok(Level::Trace),
        Ok(proto::Level::Unspecified) | Err(_) => Err(Malformed(format!("log level {value}"))),
    }
}

pub fn bootstrap_request_to_wire(request: &BootstrapRequest) -> proto::BootstrapRequest {
    proto::BootstrapRequest {
        mirror: request.mirror.as_ref().map(|mirror| proto::Mirror {
            url: mirror.url.clone(),
            key: mirror.key.clone(),
        }),
        force: request.force,
        log_level: level_to_wire(request.log_level),
    }
}

pub fn bootstrap_request_from_wire(
    request: proto::BootstrapRequest,
) -> Result<BootstrapRequest, Malformed> {
    Ok(BootstrapRequest {
        mirror: request.mirror.map(|mirror| Mirror {
            url: mirror.url,
            key: mirror.key,
        }),
        force: request.force,
        log_level: level_from_wire(request.log_level)?,
    })
}

pub fn repair_request_to_wire(request: &RepairRequest) -> proto::RepairRequest {
    proto::RepairRequest {
        log_level: level_to_wire(request.log_level),
    }
}

pub fn repair_request_from_wire(request: proto::RepairRequest) -> Result<RepairRequest, Malformed> {
    Ok(RepairRequest {
        log_level: level_from_wire(request.log_level)?,
    })
}

pub fn event_to_wire(event: Event) -> proto::Event {
    use proto::event::Kind;

    let kind = match event {
        Event::SpanOpened {
            id,
            parent,
            name,
            fields,
        } => Kind::SpanOpened(proto::SpanOpened {
            id,
            parent,
            name,
            fields: fields.into_iter().collect(),
        }),
        Event::SpanClosed { id, failed } => Kind::SpanClosed(proto::SpanClosed { id, failed }),
        Event::Log {
            level,
            span,
            message,
        } => Kind::Log(proto::Log {
            level: level_to_wire(level),
            span,
            message,
        }),
        Event::DownloadStarted { total } => Kind::DownloadStarted(proto::DownloadStarted { total }),
        Event::DownloadAdvanced { delta } => {
            Kind::DownloadAdvanced(proto::DownloadAdvanced { delta })
        }
        Event::ActivityLine(line) => Kind::ActivityLine(proto::ActivityLine { line }),
        Event::ActivityProgress(progress) => Kind::ActivityProgress(progress_to_wire(progress)),
        Event::ActivityCleared => Kind::ActivityCleared(proto::ActivityCleared {}),
        Event::Finished(outcome) => Kind::Finished(outcome_to_wire(outcome)),
    };
    proto::Event { kind: Some(kind) }
}

pub fn bootstrap_response_to_wire(event: Event) -> proto::BootstrapResponse {
    proto::BootstrapResponse {
        event: Some(event_to_wire(event)),
    }
}

pub fn bootstrap_response_from_wire(
    response: proto::BootstrapResponse,
) -> Result<Event, Malformed> {
    event_from_wire(response.event.ok_or_else(|| missing("event"))?)
}

pub fn repair_response_to_wire(event: Event) -> proto::RepairResponse {
    proto::RepairResponse {
        event: Some(event_to_wire(event)),
    }
}

pub fn repair_response_from_wire(response: proto::RepairResponse) -> Result<Event, Malformed> {
    event_from_wire(response.event.ok_or_else(|| missing("event"))?)
}

pub fn event_from_wire(event: proto::Event) -> Result<Event, Malformed> {
    use proto::event::Kind;

    Ok(
        match event.kind.ok_or_else(|| missing("an event's kind"))? {
            Kind::SpanOpened(opened) => {
                let mut fields: Vec<(String, String)> = opened.fields.into_iter().collect();
                fields.sort_unstable();
                Event::SpanOpened {
                    id: opened.id,
                    parent: opened.parent,
                    name: opened.name,
                    fields,
                }
            }
            Kind::SpanClosed(closed) => Event::SpanClosed {
                id: closed.id,
                failed: closed.failed,
            },
            Kind::Log(log) => Event::Log {
                level: level_from_wire(log.level)?,
                span: log.span,
                message: log.message,
            },
            Kind::DownloadStarted(started) => Event::DownloadStarted {
                total: started.total,
            },
            Kind::DownloadAdvanced(advanced) => Event::DownloadAdvanced {
                delta: advanced.delta,
            },
            Kind::ActivityLine(line) => Event::ActivityLine(line.line),
            Kind::ActivityProgress(progress) => {
                Event::ActivityProgress(progress_from_wire(progress))
            }
            Kind::ActivityCleared(_) => Event::ActivityCleared,
            Kind::Finished(finished) => Event::Finished(outcome_from_wire(finished)?),
        },
    )
}

fn progress_to_wire(progress: BuildProgress) -> proto::ActivityProgress {
    proto::ActivityProgress {
        builds_done: progress.builds_done,
        builds_expected: progress.builds_expected,
        builds_running: progress.builds_running,
        downloads_done: progress.downloads_done,
        downloads_expected: progress.downloads_expected,
        downloads_running: progress.downloads_running,
        bytes_done: progress.bytes_done,
        bytes_expected: progress.bytes_expected,
    }
}

fn progress_from_wire(progress: proto::ActivityProgress) -> BuildProgress {
    BuildProgress {
        builds_done: progress.builds_done,
        builds_expected: progress.builds_expected,
        builds_running: progress.builds_running,
        downloads_done: progress.downloads_done,
        downloads_expected: progress.downloads_expected,
        downloads_running: progress.downloads_running,
        bytes_done: progress.bytes_done,
        bytes_expected: progress.bytes_expected,
    }
}

fn outcome_to_wire(outcome: Outcome) -> proto::Finished {
    use proto::finished::Outcome as Wire;

    let outcome = match outcome {
        Outcome::BootstrapDone => Wire::BootstrapDone(proto::BootstrapDone {}),
        Outcome::RepairDone {
            reports,
            interrupted,
        } => Wire::RepairDone(proto::RepairDone {
            interrupted,
            reports: reports
                .into_iter()
                .map(|report| proto::RepairReport {
                    name: report.name,
                    failure: report.failure.map(target_failure_to_wire),
                })
                .collect(),
        }),
        Outcome::Failure(failure) => Wire::Failure(failure_to_wire(failure)),
    };
    proto::Finished {
        outcome: Some(outcome),
    }
}

fn outcome_from_wire(finished: proto::Finished) -> Result<Outcome, Malformed> {
    use proto::finished::Outcome as Wire;

    Ok(
        match finished.outcome.ok_or_else(|| missing("an outcome"))? {
            Wire::BootstrapDone(_) => Outcome::BootstrapDone,
            Wire::RepairDone(done) => Outcome::RepairDone {
                interrupted: done.interrupted,
                reports: done
                    .reports
                    .into_iter()
                    .map(|report| {
                        Ok(RepairReport {
                            name: report.name,
                            failure: report.failure.map(target_failure_from_wire).transpose()?,
                        })
                    })
                    .collect::<Result<_, Malformed>>()?,
            },
            Wire::Failure(failure) => Outcome::Failure(failure_from_wire(failure)?),
        },
    )
}

fn failure_to_wire(failure: Failure) -> proto::Failure {
    use proto::failure::Kind;

    let detail = |detail: String| proto::Detail { detail };
    let kind = match failure {
        Failure::Core(error) => Kind::Core(core_to_wire(error)),
        Failure::SourceBuildRequired { packages } => {
            Kind::SourceBuildRequired(proto::SourceBuildRequired {
                named: packages.is_some(),
                packages: packages.unwrap_or_default(),
            })
        }
        Failure::Network(message) => Kind::Network(detail(message)),
        Failure::Integrity { artifact, detail } => {
            Kind::Integrity(proto::Integrity { artifact, detail })
        }
        Failure::UnsupportedTarget(target) => Kind::UnsupportedTarget(detail(target)),
        Failure::Target(failure) => Kind::Target(target_failure_to_wire(failure)),
        Failure::Decompression(message) => Kind::Decompression(detail(message)),
        Failure::MalformedArchive(message) => Kind::MalformedArchive(detail(message)),
        Failure::NotRoot(what) => Kind::NotRoot(detail(what)),
        Failure::UnsupportedHost => Kind::UnsupportedHost(proto::Empty {}),
        Failure::UnsupportedKernel => Kind::UnsupportedKernel(proto::Empty {}),
        Failure::SystemdNotReady { host } => Kind::SystemdNotReady(proto::SystemdNotReady {
            host: match host {
                Host::Native => proto::Host::Native,
                Host::Wsl => proto::Host::Wsl,
            }
            .into(),
        }),
        Failure::AlreadyManaged => Kind::AlreadyManaged(proto::Empty {}),
        Failure::CrossDeviceStore { path } => Kind::CrossDeviceStore(proto::Path {
            path: path.to_string_lossy().into_owned(),
        }),
        Failure::Rollback { cause, summary } => Kind::Rollback(Box::new(proto::Rollback {
            cause: Some(Box::new(failure_to_wire(*cause))),
            summary,
        })),
        Failure::Interrupted => Kind::Interrupted(proto::Empty {}),
    };
    proto::Failure { kind: Some(kind) }
}

fn failure_from_wire(failure: proto::Failure) -> Result<Failure, Malformed> {
    use proto::failure::Kind;

    Ok(
        match failure.kind.ok_or_else(|| missing("a failure's kind"))? {
            Kind::Core(error) => Failure::Core(core_from_wire(error)?),
            Kind::SourceBuildRequired(required) => Failure::SourceBuildRequired {
                packages: required.named.then_some(required.packages),
            },
            Kind::Network(detail) => Failure::Network(detail.detail),
            Kind::Integrity(integrity) => Failure::Integrity {
                artifact: integrity.artifact,
                detail: integrity.detail,
            },
            Kind::UnsupportedTarget(detail) => Failure::UnsupportedTarget(detail.detail),
            Kind::Target(failure) => Failure::Target(target_failure_from_wire(failure)?),
            Kind::Decompression(detail) => Failure::Decompression(detail.detail),
            Kind::MalformedArchive(detail) => Failure::MalformedArchive(detail.detail),
            Kind::NotRoot(detail) => Failure::NotRoot(detail.detail),
            Kind::UnsupportedHost(_) => Failure::UnsupportedHost,
            Kind::UnsupportedKernel(_) => Failure::UnsupportedKernel,
            Kind::SystemdNotReady(not_ready) => Failure::SystemdNotReady {
                host: match proto::Host::try_from(not_ready.host) {
                    Ok(proto::Host::Native) => Host::Native,
                    Ok(proto::Host::Wsl) => Host::Wsl,
                    Ok(proto::Host::Unspecified) | Err(_) => {
                        return Err(Malformed(format!("host {}", not_ready.host)));
                    }
                },
            },
            Kind::AlreadyManaged(_) => Failure::AlreadyManaged,
            Kind::CrossDeviceStore(path) => Failure::CrossDeviceStore {
                path: path.path.into(),
            },
            Kind::Rollback(rollback) => Failure::Rollback {
                cause: Box::new(failure_from_wire(
                    *rollback
                        .cause
                        .ok_or_else(|| missing("a rollback's cause"))?,
                )?),
                summary: rollback.summary,
            },
            Kind::Interrupted(_) => Failure::Interrupted,
        },
    )
}

fn target_failure_to_wire(failure: TargetFailure) -> proto::TargetFailure {
    use proto::target_failure::Kind;

    let kind = match failure {
        TargetFailure::Core(error) => Kind::Core(core_to_wire(error)),
        TargetFailure::Unrepairable { artifact, reason } => {
            Kind::Unrepairable(proto::Unrepairable {
                artifact,
                reason: match reason {
                    Unfixable::NotADirectory => proto::Unfixable::NotADirectory,
                    Unfixable::MissingUser => proto::Unfixable::MissingUser,
                    Unfixable::MissingRuntime => proto::Unfixable::MissingRuntime,
                }
                .into(),
            })
        }
    };
    proto::TargetFailure { kind: Some(kind) }
}

fn target_failure_from_wire(failure: proto::TargetFailure) -> Result<TargetFailure, Malformed> {
    use proto::target_failure::Kind;

    Ok(
        match failure
            .kind
            .ok_or_else(|| missing("a target failure's kind"))?
        {
            Kind::Core(error) => TargetFailure::Core(core_from_wire(error)?),
            Kind::Unrepairable(unrepairable) => TargetFailure::Unrepairable {
                artifact: unrepairable.artifact,
                reason: match proto::Unfixable::try_from(unrepairable.reason) {
                    Ok(proto::Unfixable::NotADirectory) => Unfixable::NotADirectory,
                    Ok(proto::Unfixable::MissingUser) => Unfixable::MissingUser,
                    Ok(proto::Unfixable::MissingRuntime) => Unfixable::MissingRuntime,
                    Ok(proto::Unfixable::Unspecified) | Err(_) => {
                        return Err(Malformed(format!("reason {}", unrepairable.reason)));
                    }
                },
            },
        },
    )
}

fn core_to_wire(error: mix_core::Error) -> proto::CoreFailure {
    use mix_core::Error;
    use proto::core_failure::Kind;

    let path = |path: std::path::PathBuf| proto::Path {
        path: path.to_string_lossy().into_owned(),
    };
    let kind = match error {
        Error::Io { path, source } => Kind::Io(proto::Io {
            path: path.to_string_lossy().into_owned(),
            kind: kind_name(source.kind()).to_string(),
            message: source.to_string(),
        }),
        Error::Command { command, detail } => Kind::Command(proto::Command { command, detail }),
        Error::Exec { command, source } => Kind::Exec(proto::Exec {
            command,
            kind: kind_name(source.kind()).to_string(),
            message: source.to_string(),
        }),
        Error::TaskPanicked(detail) => Kind::TaskPanicked(proto::Detail { detail }),
        Error::Cancelled { command } => Kind::Cancelled(proto::Detail { detail: command }),
        Error::Locked { path: at } => Kind::Locked(path(at)),
        Error::LockMissing { path: at } => Kind::LockMissing(path(at)),
    };
    proto::CoreFailure { kind: Some(kind) }
}

fn core_from_wire(error: proto::CoreFailure) -> Result<mix_core::Error, Malformed> {
    use mix_core::Error;
    use proto::core_failure::Kind;

    Ok(
        match error.kind.ok_or_else(|| missing("a core failure's kind"))? {
            Kind::Io(io) => Error::Io {
                path: io.path.into(),
                source: std::io::Error::new(kind_from_name(&io.kind), io.message),
            },
            Kind::Command(command) => Error::Command {
                command: command.command,
                detail: command.detail,
            },
            Kind::Exec(exec) => Error::Exec {
                command: exec.command,
                source: std::io::Error::new(kind_from_name(&exec.kind), exec.message),
            },
            Kind::TaskPanicked(detail) => Error::TaskPanicked(detail.detail),
            Kind::Cancelled(detail) => Error::Cancelled {
                command: detail.detail,
            },
            Kind::Locked(path) => Error::Locked {
                path: path.path.into(),
            },
            Kind::LockMissing(path) => Error::LockMissing {
                path: path.path.into(),
            },
        },
    )
}

const KINDS: &[(ErrorKind, &str)] = &[
    (ErrorKind::NotFound, "not_found"),
    (ErrorKind::PermissionDenied, "permission_denied"),
    (ErrorKind::ConnectionRefused, "connection_refused"),
    (ErrorKind::ConnectionReset, "connection_reset"),
    (ErrorKind::ConnectionAborted, "connection_aborted"),
    (ErrorKind::NotConnected, "not_connected"),
    (ErrorKind::AddrInUse, "addr_in_use"),
    (ErrorKind::AddrNotAvailable, "addr_not_available"),
    (ErrorKind::BrokenPipe, "broken_pipe"),
    (ErrorKind::AlreadyExists, "already_exists"),
    (ErrorKind::WouldBlock, "would_block"),
    (ErrorKind::NotADirectory, "not_a_directory"),
    (ErrorKind::IsADirectory, "is_a_directory"),
    (ErrorKind::DirectoryNotEmpty, "directory_not_empty"),
    (ErrorKind::ReadOnlyFilesystem, "read_only_filesystem"),
    (ErrorKind::InvalidInput, "invalid_input"),
    (ErrorKind::InvalidData, "invalid_data"),
    (ErrorKind::TimedOut, "timed_out"),
    (ErrorKind::WriteZero, "write_zero"),
    (ErrorKind::StorageFull, "storage_full"),
    (ErrorKind::CrossesDevices, "crosses_devices"),
    (ErrorKind::Interrupted, "interrupted"),
    (ErrorKind::Unsupported, "unsupported"),
    (ErrorKind::UnexpectedEof, "unexpected_eof"),
    (ErrorKind::OutOfMemory, "out_of_memory"),
];

fn kind_name(kind: ErrorKind) -> &'static str {
    KINDS
        .iter()
        .find(|(known, _)| *known == kind)
        .map_or("other", |(_, name)| name)
}

fn kind_from_name(name: &str) -> ErrorKind {
    KINDS
        .iter()
        .find(|(_, known)| *known == name)
        .map_or(ErrorKind::Other, |(kind, _)| *kind)
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    fn through_the_wire(event: Event) -> Event {
        let bytes = event_to_wire(event).encode_to_vec();
        event_from_wire(proto::Event::decode(bytes.as_slice()).unwrap()).unwrap()
    }

    fn failure_through_the_wire(failure: Failure) -> Failure {
        match through_the_wire(Event::Finished(Outcome::Failure(failure))) {
            Event::Finished(Outcome::Failure(failure)) => failure,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn io(kind: ErrorKind, message: &str) -> std::io::Error {
        std::io::Error::new(kind, message.to_string())
    }

    fn same_core(a: &mix_core::Error, b: &mix_core::Error) -> bool {
        use mix_core::Error;
        let kind = |error: &Error| match error {
            Error::Io { source, .. } | Error::Exec { source, .. } => Some(source.kind()),
            _ => None,
        };
        a.to_string() == b.to_string()
            && kind(a) == kind(b)
            && std::mem::discriminant(a) == std::mem::discriminant(b)
    }

    fn core_errors() -> Vec<mix_core::Error> {
        use mix_core::Error;
        vec![
            Error::Io {
                path: "/nix/store".into(),
                source: io(
                    ErrorKind::PermissionDenied,
                    "permission denied (os error 13)",
                ),
            },
            Error::Command {
                command: "nix build".into(),
                detail: "error: out of disk space".into(),
            },
            Error::Exec {
                command: "useradd".into(),
                source: io(ErrorKind::NotFound, "no such file"),
            },
            Error::TaskPanicked("worker".into()),
            Error::Cancelled {
                command: "nix build".into(),
            },
            Error::Locked {
                path: "/var/lib/mix/lock".into(),
            },
            Error::LockMissing {
                path: "/var/lib/mix/lock".into(),
            },
        ]
    }

    #[test]
    fn every_core_error_survives_the_wire() {
        for error in core_errors() {
            let description = error.to_string();
            let copy = match failure_through_the_wire(Failure::Core(error_clone(&error))) {
                Failure::Core(copy) => copy,
                other => panic!("{description}: came back as {other:?}"),
            };
            assert!(same_core(&error, &copy), "{description} became {copy}");
        }
    }

    fn error_clone(error: &mix_core::Error) -> mix_core::Error {
        core_from_wire(core_to_wire_ref(error)).unwrap()
    }

    fn core_to_wire_ref(error: &mix_core::Error) -> proto::CoreFailure {
        use mix_core::Error;
        core_to_wire(match error {
            Error::Io { path, source } => Error::Io {
                path: path.clone(),
                source: io(source.kind(), &source.to_string()),
            },
            Error::Command { command, detail } => Error::Command {
                command: command.clone(),
                detail: detail.clone(),
            },
            Error::Exec { command, source } => Error::Exec {
                command: command.clone(),
                source: io(source.kind(), &source.to_string()),
            },
            Error::TaskPanicked(detail) => Error::TaskPanicked(detail.clone()),
            Error::Cancelled { command } => Error::Cancelled {
                command: command.clone(),
            },
            Error::Locked { path } => Error::Locked { path: path.clone() },
            Error::LockMissing { path } => Error::LockMissing { path: path.clone() },
        })
    }

    #[test]
    fn every_failure_kind_survives_the_wire() {
        let failures = vec![
            Failure::SourceBuildRequired {
                packages: Some(vec!["cowsay-3.8.4".into()]),
            },
            Failure::SourceBuildRequired { packages: None },
            Failure::SourceBuildRequired {
                packages: Some(Vec::new()),
            },
            Failure::Network("connection reset".into()),
            Failure::Integrity {
                artifact: "nix archive".into(),
                detail: "sha256 mismatch".into(),
            },
            Failure::UnsupportedTarget("armv7l-linux".into()),
            Failure::Target(TargetFailure::Unrepairable {
                artifact: "/nix".into(),
                reason: Unfixable::NotADirectory,
            }),
            Failure::Target(TargetFailure::Unrepairable {
                artifact: "ciuser".into(),
                reason: Unfixable::MissingUser,
            }),
            Failure::Target(TargetFailure::Unrepairable {
                artifact: "default profile".into(),
                reason: Unfixable::MissingRuntime,
            }),
            Failure::Decompression("unexpected end".into()),
            Failure::MalformedArchive("no store directory".into()),
            Failure::NotRoot("bootstrap the managed environment".into()),
            Failure::UnsupportedHost,
            Failure::UnsupportedKernel,
            Failure::SystemdNotReady { host: Host::Native },
            Failure::SystemdNotReady { host: Host::Wsl },
            Failure::AlreadyManaged,
            Failure::CrossDeviceStore {
                path: "/nix/store/pkg-a".into(),
            },
            Failure::Rollback {
                cause: Box::new(Failure::Rollback {
                    cause: Box::new(Failure::UnsupportedHost),
                    summary: "inner".into(),
                }),
                summary: "1 rollback step(s) failed".into(),
            },
            Failure::Interrupted,
        ];
        for failure in failures {
            let before = format!("{failure:?}");
            let after = format!("{:?}", failure_through_the_wire(failure));
            assert_eq!(before, after);
        }
    }

    #[test]
    fn every_event_survives_the_wire() {
        let events = vec![
            Event::SpanOpened {
                id: 7,
                parent: Some(3),
                name: "step".into(),
                fields: vec![("name".into(), "create nix dir".into())],
            },
            Event::SpanOpened {
                id: 3,
                parent: None,
                name: "rollback".into(),
                fields: Vec::new(),
            },
            Event::SpanClosed {
                id: 7,
                failed: true,
            },
            Event::Log {
                level: Level::Warn,
                span: Some(7),
                message: "Cancelling... (cleaning up)".into(),
            },
            Event::Log {
                level: Level::Trace,
                span: None,
                message: "line with \"quotes\"\nand a newline\0".into(),
            },
            Event::DownloadStarted { total: u64::MAX },
            Event::DownloadAdvanced { delta: 1 },
            Event::ActivityLine("building '/nix/store/x.drv'".into()),
            Event::ActivityProgress(BuildProgress {
                builds_done: 1,
                builds_expected: 2,
                builds_running: 3,
                downloads_done: 4,
                downloads_expected: 5,
                downloads_running: 6,
                bytes_done: 7,
                bytes_expected: 8,
            }),
            Event::ActivityCleared,
            Event::Finished(Outcome::BootstrapDone),
            Event::Finished(Outcome::RepairDone {
                interrupted: true,
                reports: vec![
                    RepairReport {
                        name: "/nix".into(),
                        failure: None,
                    },
                    RepairReport {
                        name: "/etc/nix/nix.conf".into(),
                        failure: Some(TargetFailure::Core(mix_core::Error::Command {
                            command: "chmod".into(),
                            detail: "denied".into(),
                        })),
                    },
                ],
            }),
        ];
        for event in events {
            let before = format!("{event:?}");
            assert_eq!(before, format!("{:?}", through_the_wire(event)));
        }
    }

    #[test]
    fn requests_survive_the_wire() {
        let request = BootstrapRequest {
            mirror: Some(Mirror {
                url: "http://mirror.internal".into(),
                key: Some("mix-mirror-1:AAAA".into()),
            }),
            force: true,
            log_level: Level::Debug,
        };
        let bytes = bootstrap_request_to_wire(&request).encode_to_vec();
        let decoded =
            bootstrap_request_from_wire(proto::BootstrapRequest::decode(bytes.as_slice()).unwrap());
        assert_eq!(decoded, Ok(request));

        let repair = RepairRequest {
            log_level: Level::Warn,
        };
        let bytes = repair_request_to_wire(&repair).encode_to_vec();
        let decoded =
            repair_request_from_wire(proto::RepairRequest::decode(bytes.as_slice()).unwrap());
        assert_eq!(decoded, Ok(repair));
    }

    #[test]
    fn the_wire_format_of_a_request_does_not_change_by_accident() {
        let request = BootstrapRequest {
            mirror: Some(Mirror {
                url: "http://m".into(),
                key: Some("k".into()),
            }),
            force: true,
            log_level: Level::Info,
        };

        assert_eq!(
            bootstrap_request_to_wire(&request).encode_to_vec(),
            [
                0x0a, 0x0d, 0x0a, 0x08, b'h', b't', b't', b'p', b':', b'/', b'/', b'm', 0x12, 0x01,
                b'k', 0x10, 0x01, 0x18, 0x03
            ]
        );
    }

    #[test]
    fn an_unspecified_or_unknown_level_is_refused() {
        for level in [0, 99] {
            let request = proto::RepairRequest { log_level: level };
            assert!(repair_request_from_wire(request).is_err(), "{level}");
        }
    }

    #[test]
    fn an_event_without_a_kind_is_refused() {
        assert!(event_from_wire(proto::Event { kind: None }).is_err());
        assert!(bootstrap_response_from_wire(proto::BootstrapResponse { event: None }).is_err());
        assert!(repair_response_from_wire(proto::RepairResponse { event: None }).is_err());
        assert!(
            event_from_wire(proto::Event {
                kind: Some(proto::event::Kind::Finished(proto::Finished {
                    outcome: None
                })),
            })
            .is_err()
        );
    }

    #[test]
    fn an_unknown_enum_value_in_a_failure_is_refused() {
        let failure = proto::Failure {
            kind: Some(proto::failure::Kind::SystemdNotReady(
                proto::SystemdNotReady { host: 42 },
            )),
        };
        assert!(failure_from_wire(failure).is_err());

        let unrepairable = proto::TargetFailure {
            kind: Some(proto::target_failure::Kind::Unrepairable(
                proto::Unrepairable {
                    artifact: "/nix".into(),
                    reason: 0,
                },
            )),
        };
        assert!(target_failure_from_wire(unrepairable).is_err());
    }

    #[test]
    fn a_rollback_without_its_cause_is_refused() {
        let failure = proto::Failure {
            kind: Some(proto::failure::Kind::Rollback(Box::new(proto::Rollback {
                cause: None,
                summary: "x".into(),
            }))),
        };
        assert!(failure_from_wire(failure).is_err());
    }

    #[test]
    fn an_unknown_io_error_kind_falls_back_to_other_and_keeps_its_message() {
        let error = core_from_wire(proto::CoreFailure {
            kind: Some(proto::core_failure::Kind::Io(proto::Io {
                path: "/x".into(),
                kind: "something_new".into(),
                message: "a new kind of failure".into(),
            })),
        })
        .unwrap();

        assert_eq!(error.to_string(), "I/O error at /x: a new kind of failure");
    }
}
