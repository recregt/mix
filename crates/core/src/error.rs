//! What went wrong, and nothing about what to do next.
//!
//! This is the bottom of the tool: it does not know which command is running, so it cannot know
//! what a reader should be told to try. Every variant here states a fact — the path, the command
//! line, the errno behind it — and the crate that has the context wraps it. The words a person
//! reads are written in `mix-cli`, one command at a time.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {path}: {source}")]
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

    #[error("already locked: {}", path.display())]
    Locked { path: PathBuf },
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

    /// The bottom of the tool states the fact and stops there: which command to wait for, and
    /// whether waiting is even the right advice, is not known here.
    #[test]
    fn a_raw_error_states_the_fact_without_advising_anything() {
        let err = Error::Locked {
            path: "/run/mix.lock".into(),
        };

        assert_eq!(err.to_string(), "already locked: /run/mix.lock");
    }
}
