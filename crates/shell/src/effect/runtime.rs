use std::borrow::Cow;
use std::path::PathBuf;

use mix_core::action::{Digest, Failure, Outcome, Performed};
use mix_core::{DownloadProgress, Scope};

use crate::effect::files::Prepared;
use crate::ops::bootstrap::Error;
use crate::ops::bootstrap::steps::fetch_and_unpack::{provision_runtime, remove_runtime};
use crate::ops::bootstrap::tarball;

pub fn hex(digest: &Digest) -> String {
    digest.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn failure(url: &str, error: Error) -> Failure {
    match error {
        Error::Network(_) => Failure::Network {
            url: url.to_string(),
        },
        Error::Integrity { artifact, detail } => Failure::Integrity {
            artifact,
            expected: "the pinned archive".to_string(),
            found: detail,
        },
        Error::Interrupted => Failure::Cancelled,
        Error::CrossDeviceStore { path } => Failure::Io {
            path,
            kind: std::io::ErrorKind::CrossesDevices,
        },
        Error::Core(mix_core::Error::Io { path, source }) => Failure::Io {
            path,
            kind: source.kind(),
        },
        Error::Core(mix_core::Error::Cancelled { .. }) => Failure::Cancelled,
        other => Failure::CommandFailed {
            program: "provision the runtime".to_string(),
            status: None,
            output_tail: other.to_string(),
        },
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| Failure::CommandFailed {
            program: "provision the runtime".to_string(),
            status: None,
            output_tail: error.to_string(),
        })
}

pub async fn install(
    url: &str,
    sha256: &Digest,
    size: u64,
    progress: &dyn DownloadProgress,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Outcome {
    let bytes: Cow<'static, [u8]> = match tarball::embedded() {
        Some(bytes) => Cow::Borrowed(bytes),
        None => Cow::Owned(
            tarball::fetch_and_verify(url, &hex(sha256), size, progress, scope)
                .await
                .map_err(|error| failure(url, error))?,
        ),
    };
    let profile = std::path::Path::new(mix_core::paths::DEFAULT_PROFILE_BIN)
        .parent()
        .expect("the profile's bin has a parent");
    let predicted: Vec<PathBuf> = [profile, std::path::Path::new(mix_core::paths::NIX_STORE)]
        .into_iter()
        .filter(|path| !path.exists())
        .map(std::path::Path::to_path_buf)
        .collect();
    prepared(&[mix_core::action::Action::RemoveRuntime { created: predicted }])?;
    let working = scope.clone();
    let (created, provisioned) = blocking(move || provision_runtime(&bytes, &working)).await?;
    let failed = match provisioned {
        Ok(()) if !scope.is_stopped() => {
            return Ok(Performed {
                undo: vec![mix_core::action::Action::RemoveRuntime { created }],
            });
        }
        Ok(()) => Failure::Cancelled,
        Err(error) => failure(url, error),
    };
    let cleanup = created.clone();
    if let Err(error) = blocking(move || remove_runtime(&cleanup)).await? {
        tracing::warn!("could not remove a partly installed runtime: {error}");
    }
    Err(failed)
}

pub async fn remove(created: &[PathBuf]) -> Outcome {
    let created = created.to_vec();
    blocking(move || remove_runtime(&created))
        .await?
        .map_err(|error| failure("", error))?;
    Ok(Performed { undo: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_is_written_as_the_pins_write_it() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x0c;
        bytes[31] = 0x58;

        let written = hex(&Digest(bytes));

        assert_eq!(written.len(), 64);
        assert!(written.starts_with("0c00"));
        assert!(written.ends_with("0058"));
    }

    #[test]
    fn download_problems_become_typed_failures() {
        assert_eq!(
            failure("https://mirror/nix.tar.xz", Error::Network("down".into())),
            Failure::Network {
                url: "https://mirror/nix.tar.xz".into()
            }
        );
        assert!(matches!(
            failure(
                "u",
                Error::Integrity {
                    artifact: "u".into(),
                    detail: "8 bytes".into()
                }
            ),
            Failure::Integrity { .. }
        ));
        assert_eq!(failure("u", Error::Interrupted), Failure::Cancelled);
    }
}
