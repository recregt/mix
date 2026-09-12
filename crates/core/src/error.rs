use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("command `{command}` failed: {detail}")]
    Command { command: String, detail: String },

    #[error(
        "network request failed: {0}\n\
         Please check your network connection, proxy settings, or --mirror URL."
    )]
    Network(String),

    #[error("{artifact}: {detail}")]
    Integrity { artifact: String, detail: String },

    #[error("`mix` does not support this platform ({0})")]
    UnsupportedTarget(String),

    #[error("decompressing archive: {0}")]
    Decompression(String),

    #[error("archive layout was not what `mix` expected: {0}")]
    MalformedArchive(String),

    #[error("background task panicked: {0}")]
    TaskPanicked(String),
}

pub type Result<T> = std::result::Result<T, Error>;
