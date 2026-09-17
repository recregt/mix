#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error(
        "network request failed: {0}\n\
         Please check your network connection, proxy settings, or --mirror URL."
    )]
    Network(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("{artifact}: {detail}")]
    Integrity { artifact: String, detail: String },

    #[error("unsupported platform: {0}")]
    UnsupportedTarget(String),

    #[error(transparent)]
    Repair(#[from] crate::repair::Error),

    #[error("decompressing archive: {0}")]
    Decompression(String),

    #[error("unexpected archive layout: {0}")]
    MalformedArchive(String),

    #[error(
        "root privileges required to {0}.\n\
         Please re-run this command with sudo:\n\
         \x20 sudo mix ..."
    )]
    NotRoot(&'static str),

    #[error(
        "this system already manages its own environment natively.\n\
         `mix` is designed for standard Linux distributions and is not needed on NixOS."
    )]
    UnsupportedHost,

    #[error(
        "a real Linux kernel is required for sandboxed builds (WSL1 is not supported).\n\
         To upgrade this distro to WSL2, run from Windows PowerShell:\n\
         \x20 wsl --set-version <distro> 2"
    )]
    UnsupportedKernel,

    #[error("{hint}")]
    SystemdNotReady { hint: &'static str },

    #[error(
        "an existing, unmanaged runtime was detected on this system.\n\
         `mix` requires a dedicated environment to manage its own reproducible runtime.\n\
         To continue, uninstall the existing Nix installation or remove `/nix`:\n\
         \x20 sudo rm -rf /nix"
    )]
    AlreadyManaged,

    #[error(
        "cannot move {path} into place: it is on a different filesystem than /nix.\n\
         `mix` stages packages under /nix and moves them into /nix/store with an atomic \
         rename, which requires both to be on the same filesystem. Remove any separate \
         mount at /nix/store (e.g. a custom fstab entry) and retry."
    )]
    CrossDeviceStore { path: std::path::PathBuf },

    #[error("{cause}\n{summary}")]
    Rollback {
        #[source]
        cause: Box<Error>,
        summary: String,
    },

    #[error("{}", source_build_message(.0))]
    SourceBuildRequired(Vec<String>),

    #[error("interrupted; rolled back any partially applied changes")]
    Interrupted,
}

/// How many derivations are worth naming before the list stops being readable.
const NAMED_SOURCE_BUILDS: usize = 5;

fn source_build_message(derivations: &[String]) -> String {
    let named: Vec<&str> = derivations
        .iter()
        .take(NAMED_SOURCE_BUILDS)
        .map(String::as_str)
        .collect();
    let mut list = named.join(", ");
    if let Some(rest) = derivations
        .len()
        .checked_sub(named.len())
        .filter(|n| *n > 0)
    {
        list.push_str(&format!(" and {rest} more"));
    }

    format!(
        "the binary cache has nothing to download for: {list}\n\
         Installing this would compile it from source, which can take hours.\n\
         To compile it anyway, re-run with --build:\n\
         \x20 mix install --build ..."
    )
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_preserves_the_source_chain_and_the_mirror_hint() {
        let boxed: Box<dyn std::error::Error + Send + Sync> = "connection reset".into();
        let err = Error::Network(boxed);

        assert!(err.to_string().contains("--mirror"));
        let source = std::error::Error::source(&err).expect("source should be preserved");
        assert_eq!(source.to_string(), "connection reset");
    }

    #[test]
    fn cross_device_store_names_the_offending_path_and_the_fix() {
        let err = Error::CrossDeviceStore {
            path: "/nix/store/pkg-a".into(),
        };

        let message = err.to_string();
        assert!(message.contains("/nix/store/pkg-a"));
        assert!(message.contains("fstab"));
    }

    #[test]
    fn source_build_error_names_the_derivations_and_the_way_out() {
        let err = Error::SourceBuildRequired(vec!["hello-2.12.3".to_string()]);

        let message = err.to_string();
        assert!(message.contains("hello-2.12.3"));
        assert!(message.contains("mix install --build"));
    }

    #[test]
    fn source_build_error_counts_the_derivations_it_does_not_name() {
        let derivations: Vec<String> = (0..8).map(|i| format!("package-{i}")).collect();
        let message = Error::SourceBuildRequired(derivations).to_string();

        assert!(message.contains("package-4"));
        assert!(!message.contains("package-5"));
        assert!(message.contains("and 3 more"));
    }

    #[test]
    fn rollback_error_reports_the_original_cause_and_the_cleanup_summary() {
        let err = Error::Rollback {
            cause: Box::new(Error::UnsupportedHost),
            summary: "1 rollback step(s) failed, the system may need manual cleanup: nixbld group: exit 1".to_string(),
        };

        let message = err.to_string();
        assert!(message.contains("not needed on NixOS"));
        assert!(message.contains("manual cleanup"));
        let source = std::error::Error::source(&err).expect("cause should be preserved");
        assert!(source.to_string().contains("not needed on NixOS"));
    }
}
