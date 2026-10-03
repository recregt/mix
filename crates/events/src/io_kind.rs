use std::io::ErrorKind;

pub const KINDS: &[ErrorKind] = &[
    ErrorKind::NotFound,
    ErrorKind::PermissionDenied,
    ErrorKind::ConnectionRefused,
    ErrorKind::ConnectionReset,
    ErrorKind::HostUnreachable,
    ErrorKind::NetworkUnreachable,
    ErrorKind::ConnectionAborted,
    ErrorKind::NotConnected,
    ErrorKind::AddrInUse,
    ErrorKind::AddrNotAvailable,
    ErrorKind::NetworkDown,
    ErrorKind::BrokenPipe,
    ErrorKind::AlreadyExists,
    ErrorKind::WouldBlock,
    ErrorKind::NotADirectory,
    ErrorKind::IsADirectory,
    ErrorKind::DirectoryNotEmpty,
    ErrorKind::ReadOnlyFilesystem,
    ErrorKind::StaleNetworkFileHandle,
    ErrorKind::InvalidInput,
    ErrorKind::InvalidData,
    ErrorKind::TimedOut,
    ErrorKind::WriteZero,
    ErrorKind::StorageFull,
    ErrorKind::NotSeekable,
    ErrorKind::QuotaExceeded,
    ErrorKind::FileTooLarge,
    ErrorKind::ResourceBusy,
    ErrorKind::ExecutableFileBusy,
    ErrorKind::Deadlock,
    ErrorKind::CrossesDevices,
    ErrorKind::TooManyLinks,
    ErrorKind::InvalidFilename,
    ErrorKind::ArgumentListTooLong,
    ErrorKind::Interrupted,
    ErrorKind::Unsupported,
    ErrorKind::UnexpectedEof,
    ErrorKind::OutOfMemory,
    ErrorKind::Other,
];

pub fn name(kind: ErrorKind) -> String {
    format!("{kind:?}")
}

pub fn named(name: &str) -> ErrorKind {
    KINDS
        .iter()
        .copied()
        .find(|kind| self::name(*kind) == name)
        .unwrap_or(ErrorKind::Other)
}
