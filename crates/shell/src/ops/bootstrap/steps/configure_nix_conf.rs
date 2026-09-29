use std::path::{Path, PathBuf};

use async_trait::async_trait;
use mix_core::{Scope, Step};

use mix_core::models::PROFILE_SNIPPET;
use mix_core::paths::{NIX_CONF_DEST, NIX_DAEMON_SERVICE_UNIT, POLICY_FILE, PROFILE_SNIPPET_DEST};
use mix_core::policy::Policy;

use crate::effect::fs::{create_dir_all, exists, remove_dir_all, remove_file, write_atomic};
use crate::effect::systemd::restart_if_active;
use crate::ops::bootstrap::cleanup::warn_on_failure;
use crate::ops::bootstrap::error::{Error, Result};

pub struct ConfigureNixConf {
    policy: Policy,
    written: Vec<WrittenFile>,
}

impl ConfigureNixConf {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            written: Vec::new(),
        }
    }

    fn files(&self) -> [(&'static str, String); 3] {
        [
            (POLICY_FILE, self.policy.render()),
            (NIX_CONF_DEST, self.policy.nix_conf()),
            (PROFILE_SNIPPET_DEST, PROFILE_SNIPPET.to_string()),
        ]
    }
}

struct WrittenFile {
    path: &'static str,
    previous: Option<Vec<u8>>,
    created_dir: Option<PathBuf>,
}

#[async_trait]
impl Step for ConfigureNixConf {
    type Error = Error;

    fn name(&self) -> &'static str {
        "write runtime configuration"
    }

    async fn check(&self, _scope: &Scope) -> Result<bool> {
        for (path, expected) in self.files() {
            if !matches_expected(path, &expected).await {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn execute(&mut self, scope: &Scope) -> Result<()> {
        let mut restart_daemon = false;
        for (path, contents) in self.files() {
            let previous = previous_contents(path).await;
            if path == NIX_CONF_DEST {
                restart_daemon = daemon_needs_the_new_config(previous.as_deref(), &contents);
            }
            let created_dir = write(path, &contents).await?;
            self.written.push(WrittenFile {
                path,
                previous,
                created_dir,
            });
        }

        if restart_daemon {
            restart_running_daemon(scope).await;
        }

        Ok(())
    }

    async fn rollback(&mut self, _scope: &Scope) -> Result<()> {
        for written in self.written.drain(..).rev() {
            warn_on_failure(
                "restore runtime configuration file",
                match written.previous {
                    Some(contents) => write_atomic(written.path, contents).await,
                    None => remove_file(written.path).await,
                },
            );
            if let Some(dir) = written.created_dir {
                warn_on_failure("remove created directory", remove_dir_all(dir).await);
            }
        }
        Ok(())
    }
}

fn daemon_needs_the_new_config(previous: Option<&[u8]>, expected: &str) -> bool {
    previous != Some(expected.as_bytes())
}

/// nix-daemon reads `trusted-users` once, at startup, so a rewritten nix.conf
/// only reaches a running daemon if it is restarted.
async fn restart_running_daemon(scope: &Scope) {
    warn_on_failure(
        "restart nix-daemon",
        restart_if_active(NIX_DAEMON_SERVICE_UNIT, scope).await,
    );
}

async fn matches_expected(path: &str, expected: &str) -> bool {
    tokio::fs::read_to_string(path)
        .await
        .map(|s| s == expected)
        .unwrap_or(false)
}

async fn previous_contents(path: &str) -> Option<Vec<u8>> {
    tokio::fs::read(path).await.ok()
}

async fn write(path: &str, contents: &str) -> Result<Option<PathBuf>> {
    let created_dir = match Path::new(path).parent() {
        Some(dir) => {
            let created_dir = first_missing_ancestor(dir).await;
            create_dir_all(dir).await?;
            created_dir
        }
        None => None,
    };
    write_atomic(path, contents).await?;
    Ok(created_dir)
}

async fn first_missing_ancestor(dir: &Path) -> Option<PathBuf> {
    let mut missing = None;
    let mut candidate = dir;
    while !exists(candidate).await {
        missing = Some(candidate.to_path_buf());
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => break,
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nix_conf() -> String {
        Policy::default().nix_conf()
    }

    #[test]
    fn a_missing_nix_conf_means_the_daemon_needs_the_new_config() {
        assert!(daemon_needs_the_new_config(None, &nix_conf()));
    }

    #[test]
    fn a_drifted_nix_conf_means_the_daemon_needs_the_new_config() {
        assert!(daemon_needs_the_new_config(
            Some(b"trusted-users = root alice\n"),
            &nix_conf()
        ));
    }

    #[test]
    fn an_identical_nix_conf_leaves_the_daemon_alone() {
        assert!(!daemon_needs_the_new_config(
            Some(nix_conf().as_bytes()),
            &nix_conf()
        ));
    }

    #[tokio::test]
    async fn matches_expected_is_true_for_identical_contents() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nix.conf");
        tokio::fs::write(&file, nix_conf()).await.unwrap();

        assert!(matches_expected(file.to_str().unwrap(), &nix_conf()).await);
    }

    #[tokio::test]
    async fn matches_expected_detects_a_dropped_trusted_users_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nix.conf");
        let without_trust: String = nix_conf()
            .lines()
            .filter(|line| !line.starts_with("trusted-users"))
            .map(|line| format!("{line}\n"))
            .collect();
        tokio::fs::write(&file, &without_trust).await.unwrap();

        assert!(!matches_expected(file.to_str().unwrap(), &nix_conf()).await);
    }

    #[tokio::test]
    async fn matches_expected_is_false_when_the_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent").join("nix.conf");

        assert!(!matches_expected(missing.to_str().unwrap(), &nix_conf()).await);
    }

    #[tokio::test]
    async fn first_missing_ancestor_is_none_when_the_directory_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(first_missing_ancestor(dir.path()).await, None);
    }

    #[tokio::test]
    async fn first_missing_ancestor_finds_the_topmost_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b").join("c");
        assert_eq!(
            first_missing_ancestor(&target).await,
            Some(dir.path().join("a"))
        );
    }

    #[tokio::test]
    async fn write_creates_missing_parents_and_reports_the_topmost_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("b").join("nix.conf");

        let created_dir = write(file.to_str().unwrap(), "content").await.unwrap();

        assert_eq!(created_dir, Some(dir.path().join("a")));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "content");
    }

    #[tokio::test]
    async fn write_reports_no_created_dir_when_the_parent_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nix.conf");

        let created_dir = write(file.to_str().unwrap(), "content").await.unwrap();

        assert_eq!(created_dir, None);
    }

    #[tokio::test]
    async fn rollback_removes_a_directory_it_created_for_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("nix.conf");
        let created_dir = write(file.to_str().unwrap(), "content").await.unwrap();

        let mut step = ConfigureNixConf {
            policy: Policy::default(),
            written: vec![WrittenFile {
                path: Box::leak(file.to_str().unwrap().to_string().into_boxed_str()),
                previous: None,
                created_dir,
            }],
        };
        step.rollback(&mix_exec::Scope::root()).await.unwrap();

        assert!(!dir.path().join("a").exists());
    }
}
