//! The declarative targets: what mix says the system should look like, measured and reconciled.
//!
//! A [`Target`](mix_core::models::Target) is a fact about the system mix declares — a directory
//! with a mode, an account with a pair of ids, a unit that has to be running. Two things can be
//! done with one: it can be [`inspect`]ed, which measures it and says what drifted, and it can
//! be [`reconcile`]d, which puts it back.
//!
//! Both halves live here rather than one inside `mix doctor` and the other inside `mix repair`.
//! They used to be written twice, and the copies measured the same artifact in different ways:
//! an audit said `user is missing or has the wrong uid/gid` while repair looked the account up
//! again to find out which. Now the measurement is taken once and the reconciliation starts from
//! it, so the two commands cannot disagree, and repair pays for one lookup per target instead of
//! one per question it asks about it.

mod finding;
mod inspect;
mod reconcile;

pub use finding::{Finding, Unfixable};
pub use inspect::inspect;
pub use reconcile::reconcile;

use mix_core::CancellationToken;
use mix_core::models::Target;

/// What reconciling a target could not do.
///
/// Facts and the context around them: which artifact, and — for something beyond repair's
/// reach — which of the reasons it is out of reach for. The words are `mix-cli`'s.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error("{artifact}: {reason}")]
    Unrepairable { artifact: String, reason: Unfixable },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Measures one target and, if it drifted, puts it back. Says whether anything was done.
///
/// The measurement decides both halves of that: whether there is anything to do, and whether it
/// is something repair can do at all — [`Finding::unfixable`] is what binds a finding to the
/// reason it cannot, so a caller never has to guess what it can promise a reader.
pub async fn apply(target: &Target, token: &CancellationToken) -> Result<bool> {
    let Some(finding) = inspect(target).await else {
        return Ok(false);
    };
    if let Some(reason) = finding.unfixable() {
        return Err(Error::Unrepairable {
            artifact: target.label().into_owned(),
            reason,
        });
    }
    reconcile(target, finding, token).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn apply_does_nothing_to_a_target_that_is_already_as_declared() {
        let dir = tempfile::tempdir().unwrap();
        let target = Target::Directory {
            path: dir.path().to_path_buf(),
            mode: std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(dir.path()).unwrap().permissions(),
            ) & 0o7777,
            owner: None,
        };

        assert!(!apply(&target, &CancellationToken::new()).await.unwrap());
    }

    #[tokio::test]
    async fn apply_reports_the_artifact_it_could_not_repair() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let target = Target::Directory {
            path: file.clone(),
            mode: 0o755,
            owner: None,
        };

        let error = apply(&target, &CancellationToken::new()).await.unwrap_err();

        match error {
            Error::Unrepairable { artifact, reason } => {
                assert_eq!(artifact, file.to_string_lossy());
                assert_eq!(reason, Unfixable::NotADirectory);
            }
            other => panic!("expected an unrepairable artifact, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_creates_a_declared_directory_that_is_not_there() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nix/var/nix/profiles");
        let target = Target::Directory {
            path: path.clone(),
            mode: 0o755,
            owner: None,
        };

        assert!(apply(&target, &CancellationToken::new()).await.unwrap());
        assert!(path.is_dir());
    }

    /// The runtime is bound to a reason rather than reconciled, so a caller is never told it was
    /// repaired.
    #[tokio::test]
    async fn apply_refuses_a_missing_part_of_the_nix_runtime() {
        let target = Target::PathExists {
            name: "default profile",
            path: "/does/not/exist/nix-env",
        };

        let error = apply(&target, &CancellationToken::new()).await.unwrap_err();

        assert!(matches!(
            error,
            Error::Unrepairable {
                reason: Unfixable::MissingRuntime,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn apply_leaves_a_file_mix_does_not_write_alone() {
        let dir = tempfile::tempdir().unwrap();
        let target = Target::File {
            path: dir.path().join("flake.lock"),
            expected: None,
            owner: None,
        };

        assert!(!apply(&target, &CancellationToken::new()).await.unwrap());
    }
}
