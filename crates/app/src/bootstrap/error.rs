//! What bootstrap could not do, and the context that makes it legible.
//!
//! A variant here says which artifact, which host, which derivations — the facts `mix-core` does
//! not have and this crate does. What a reader should do about it depends on the command they
//! ran, which this crate does not know, so the advice is written in `mix-cli` instead.

/// Where systemd was looked for, which is what decides how it is turned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// An ordinary Linux distribution.
    Native,
    /// A WSL distro, where systemd is opt-in.
    Wsl,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    /// The profile would not activate: bootstrap's last step is the same one `mix install` runs.
    #[error(transparent)]
    Activation(#[from] crate::profile::Error),

    #[error("network request failed: {0}")]
    Network(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("{artifact}: {detail}")]
    Integrity { artifact: String, detail: String },

    #[error("unsupported platform: {0}")]
    UnsupportedTarget(String),

    #[error(transparent)]
    Target(#[from] crate::target::Error),

    #[error("decompressing archive: {0}")]
    Decompression(String),

    #[error("unexpected archive layout: {0}")]
    MalformedArchive(String),

    #[error("root privileges are required to {0}")]
    NotRoot(&'static str),

    #[error("this host is NixOS, which manages its own environment natively")]
    UnsupportedHost,

    #[error("WSL1 does not provide the real Linux kernel that sandboxed builds need")]
    UnsupportedKernel,

    #[error("systemd is not active: `/run/systemd/system` is missing or PID 1 is not systemd")]
    SystemdNotReady { host: Host },

    #[error("an existing, unmanaged Nix installation was found on this system")]
    AlreadyManaged,

    #[error(
        "cannot move {} into /nix/store: it is on a different filesystem",
        path.display()
    )]
    CrossDeviceStore { path: std::path::PathBuf },

    #[error("{cause}\n{summary}")]
    Rollback {
        #[source]
        cause: Box<Error>,
        summary: String,
    },

    #[error("interrupted; rolled back any partially applied changes")]
    Interrupted,
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_preserves_the_source_chain() {
        let boxed: Box<dyn std::error::Error + Send + Sync> = "connection reset".into();
        let err = Error::Network(boxed);

        let source = std::error::Error::source(&err).expect("source should be preserved");
        assert_eq!(source.to_string(), "connection reset");
    }

    /// The path is the context this crate has; what to do about the mount it is on is the
    /// command's business, not the library's.
    #[test]
    fn cross_device_store_names_the_offending_path() {
        let err = Error::CrossDeviceStore {
            path: "/nix/store/pkg-a".into(),
        };

        assert_eq!(
            err.to_string(),
            "cannot move /nix/store/pkg-a into /nix/store: it is on a different filesystem"
        );
    }

    /// An activation failure is carried as it was raised: bootstrap adds nothing to it, because
    /// the same fact reaches a reader who ran `mix install` too.
    #[test]
    fn an_activation_failure_keeps_the_words_the_profile_layer_raised() {
        let err = Error::Activation(crate::profile::Error::SourceBuildRequired {
            packages: Some(vec!["hello-2.12.3".into(), "cowsay-3.8.4".into()]),
        });

        assert_eq!(
            err.to_string(),
            "the binary cache has nothing to download for: hello-2.12.3, cowsay-3.8.4"
        );
    }

    #[test]
    fn a_refusal_with_nothing_to_name_still_says_what_was_refused() {
        for packages in [None, Some(Vec::new())] {
            let err = Error::Activation(crate::profile::Error::SourceBuildRequired { packages });

            assert_eq!(
                err.to_string(),
                "the binary cache cannot serve everything this build needs"
            );
        }
    }

    #[test]
    fn rollback_error_reports_the_original_cause_and_the_cleanup_summary() {
        let err = Error::Rollback {
            cause: Box::new(Error::UnsupportedHost),
            summary: "1 rollback step(s) failed: nixbld group: exit 1".to_string(),
        };

        let message = err.to_string();
        assert!(message.contains("NixOS"));
        assert!(message.contains("nixbld group: exit 1"));
        let source = std::error::Error::source(&err).expect("cause should be preserved");
        assert!(source.to_string().contains("NixOS"));
    }
}
