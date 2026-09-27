#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("running `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("command `{command}` was interrupted")]
    Cancelled { command: String },

    #[error("command `{command}` failed: {detail}")]
    Failed { command: String, detail: String },
}
