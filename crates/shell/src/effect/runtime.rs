use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mix_core::action::{Digest, Failure, Outcome, Performed};
use mix_core::{DownloadProgress, Scope};

use crate::effect::files::Prepared;
use crate::ops::bootstrap::Error;
use crate::ops::bootstrap::steps::fetch_and_unpack::{provision_runtime, remove_runtime};
use crate::ops::bootstrap::tarball;

pub fn listing(root: &Path, skip: &Path) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path == skip {
                continue;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(path.clone());
            }
            found.insert(path);
        }
    }
    found
}

pub fn added(before: &BTreeSet<PathBuf>, after: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    after
        .iter()
        .filter(|path| !before.contains(*path))
        .filter(|path| {
            path.parent()
                .is_none_or(|parent| !after.contains(parent) || before.contains(parent))
        })
        .cloned()
        .collect()
}

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
    let nix = Path::new("/nix");
    let store = Path::new(mix_core::paths::NIX_STORE);
    let before = listing(nix, store);
    let working = scope.clone();
    let (mut created, provisioned) = blocking(move || provision_runtime(&bytes, &working)).await?;
    for path in added(&before, &listing(nix, store)) {
        if !created.iter().any(|known| path.starts_with(known)) {
            created.push(path);
        }
    }
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
    fn only_the_topmost_new_paths_are_recorded() {
        let before: BTreeSet<PathBuf> = ["/nix/var", "/nix/var/nix", "/nix/var/nix/db"]
            .map(PathBuf::from)
            .into();
        let after: BTreeSet<PathBuf> = [
            "/nix/var",
            "/nix/var/nix",
            "/nix/var/nix/db",
            "/nix/var/nix/db/db.sqlite",
            "/nix/var/nix/profiles",
            "/nix/var/nix/profiles/default",
            "/nix/var/nix/profiles/default-1-link",
        ]
        .map(PathBuf::from)
        .into();

        assert_eq!(
            added(&before, &after),
            ["/nix/var/nix/db/db.sqlite", "/nix/var/nix/profiles"].map(PathBuf::from)
        );
    }

    #[test]
    fn a_listing_skips_the_store_and_does_not_follow_links() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("store/big")).unwrap();
        std::fs::create_dir_all(root.path().join("var/nix")).unwrap();
        std::os::unix::fs::symlink(root.path().join("store"), root.path().join("var/link"))
            .unwrap();

        let found = listing(root.path(), &root.path().join("store"));

        assert_eq!(
            found,
            ["var", "var/link", "var/nix"]
                .map(|path| root.path().join(path))
                .into()
        );
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
