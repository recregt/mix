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

    #[error("running `{command}`: {source}")]
    Exec {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("background task panicked: {0}")]
    TaskPanicked(String),

    #[error("command `{command}` was interrupted")]
    Cancelled { command: String },
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_error_preserves_the_source_chain() {
        let source = std::io::Error::from(std::io::ErrorKind::NotFound);
        let err = Error::Exec {
            command: "nix-store --load-db".into(),
            source,
        };

        let source = std::error::Error::source(&err).expect("source should be preserved");
        assert_eq!(source.to_string(), "entity not found");
    }
}
