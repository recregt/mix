use std::path::{Path, PathBuf};

use mix_core::paths::{FLAKE_LOCK, FLAKE_NIX, HOME_NIX};
use mix_core::privilege::InvokingUser;
use mix_core::{CancellationToken, Error, Result};

use crate::shared::os::{path_exists, run, run_as, status_as};

const AUTHOR_NAME: &str = "mix";
const AUTHOR_EMAIL: &str = "mix@localhost";
const COMMIT_MESSAGE: &str = "mix: sync generated home-manager config";

const GIT_BINARY_ENV: &str = "MIX_GIT_PATH";
const PROFILE_GIT: &str = ".nix-profile/bin/git";
const GITIGNORE: &str = ".gitignore";

const MANAGED_FILES: &[&str] = &[GITIGNORE, FLAKE_LOCK, FLAKE_NIX, HOME_NIX];

const GITIGNORE_CONTENTS: &str = "# Managed by mix -- do not edit, changes are overwritten.\n/*\n!/.gitignore\n!/flake.lock\n!/flake.nix\n!/home.nix\n";

pub struct Git {
    binary: String,
}

impl Git {
    pub async fn resolve(user: &InvokingUser) -> Self {
        Self {
            binary: resolve_binary(user, std::env::var(GIT_BINARY_ENV).ok()).await,
        }
    }

    pub async fn init(
        &self,
        user: &InvokingUser,
        state_dir: &Path,
        token: &CancellationToken,
    ) -> Result<()> {
        let state_dir_str = state_dir.to_string_lossy().into_owned();
        self.run_as(user, &["-C", &state_dir_str, "init", "-q"], token)
            .await?;
        write_gitignore(user, state_dir).await
    }

    pub async fn sync(
        &self,
        user: &InvokingUser,
        state_dir: &Path,
        token: &CancellationToken,
    ) -> Result<bool> {
        let git_dir = state_dir.join(".git");
        if !path_exists(&git_dir).await {
            return Ok(false);
        }
        run(
            "chown",
            &[
                "-R",
                &format!("{}:{}", user.uid, user.gid),
                &git_dir.to_string_lossy(),
            ],
            token,
        )
        .await?;

        let state_dir_str = state_dir.to_string_lossy().into_owned();
        let staged = self.stage(user, &state_dir_str, state_dir, token).await?;
        if !staged {
            return Ok(false);
        }

        let clean = self
            .status_as(
                user,
                &["-C", &state_dir_str, "diff", "--cached", "--quiet"],
                token,
            )
            .await?;
        if clean {
            return Ok(false);
        }

        self.run_as(
            user,
            &[
                "-c",
                &format!("user.name={AUTHOR_NAME}"),
                "-c",
                &format!("user.email={AUTHOR_EMAIL}"),
                "-C",
                &state_dir_str,
                "commit",
                "-q",
                "-m",
                COMMIT_MESSAGE,
            ],
            token,
        )
        .await?;

        Ok(true)
    }

    /// Stages the generated config and nothing else: anything a user drops next
    /// to it stays out of mix's commits.
    async fn stage(
        &self,
        user: &InvokingUser,
        state_dir_str: &str,
        state_dir: &Path,
        token: &CancellationToken,
    ) -> Result<bool> {
        let mut args = vec!["-C", state_dir_str, "ls-files", "--"];
        args.extend(MANAGED_FILES);
        let tracked = self.run_as(user, &args, token).await?;

        let mut paths: Vec<&str> = Vec::new();
        for file in MANAGED_FILES {
            if tracked.lines().any(|line| line == *file) || path_exists(state_dir.join(file)).await
            {
                paths.push(file);
            }
        }
        if paths.is_empty() {
            return Ok(false);
        }

        let mut args = vec!["-C", state_dir_str, "add", "-A", "--"];
        args.extend(&paths);
        self.run_as(user, &args, token).await?;
        Ok(true)
    }

    async fn run_as(
        &self,
        user: &InvokingUser,
        args: &[&str],
        token: &CancellationToken,
    ) -> Result<String> {
        run_as(user, &self.binary, args, token).await
    }

    async fn status_as(
        &self,
        user: &InvokingUser,
        args: &[&str],
        token: &CancellationToken,
    ) -> Result<bool> {
        status_as(user, &self.binary, args, token).await
    }
}

async fn resolve_binary(user: &InvokingUser, configured: Option<String>) -> String {
    if let Some(binary) = configured.map(|path| path.trim().to_string())
        && !binary.is_empty()
    {
        return binary;
    }
    let profile_git = profile_git(user);
    if path_exists(&profile_git).await {
        return profile_git.to_string_lossy().into_owned();
    }
    "git".to_string()
}

fn profile_git(user: &InvokingUser) -> PathBuf {
    user.home.join(PROFILE_GIT)
}

async fn write_gitignore(user: &InvokingUser, state_dir: &Path) -> Result<()> {
    let path = state_dir.join(GITIGNORE);
    tokio::fs::write(&path, GITIGNORE_CONTENTS)
        .await
        .map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })?;
    nix::unistd::chown(
        &path,
        Some(nix::unistd::Uid::from_raw(user.uid)),
        Some(nix::unistd::Gid::from_raw(user.gid)),
    )
    .map_err(|e| Error::Io {
        path,
        source: std::io::Error::from(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(home: &Path) -> InvokingUser {
        InvokingUser {
            uid: nix::unistd::Uid::current().as_raw(),
            gid: nix::unistd::Gid::current().as_raw(),
            name: "mix-user".to_string(),
            home: home.to_path_buf(),
        }
    }

    fn git() -> Git {
        Git {
            binary: "git".to_string(),
        }
    }

    async fn repository(home: &Path) -> PathBuf {
        let state_dir = mix_core::paths::mix_state_dir(home);
        tokio::fs::create_dir_all(&state_dir).await.unwrap();
        git()
            .init(&user(home), &state_dir, &CancellationToken::new())
            .await
            .unwrap();
        state_dir
    }

    async fn committed_files(state_dir: &Path) -> Vec<String> {
        let output = tokio::process::Command::new("git")
            .args(["-C", &state_dir.to_string_lossy(), "ls-files"])
            .output()
            .await
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn resolve_binary_prefers_the_configured_override() {
        let home = tempfile::tempdir().unwrap();
        let binary = resolve_binary(&user(home.path()), Some(" /usr/bin/git ".to_string())).await;
        assert_eq!(binary, "/usr/bin/git");
    }

    #[tokio::test]
    async fn resolve_binary_uses_the_git_in_the_users_nix_profile() {
        let home = tempfile::tempdir().unwrap();
        let profile_git = profile_git(&user(home.path()));
        std::fs::create_dir_all(profile_git.parent().unwrap()).unwrap();
        std::fs::write(&profile_git, "").unwrap();

        let binary = resolve_binary(&user(home.path()), None).await;

        assert_eq!(binary, profile_git.to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_binary_falls_back_to_the_path() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(resolve_binary(&user(home.path()), None).await, "git");
        assert_eq!(
            resolve_binary(&user(home.path()), Some("   ".to_string())).await,
            "git"
        );
    }

    #[tokio::test]
    async fn sync_is_a_noop_for_a_state_dir_that_is_not_a_repository() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = mix_core::paths::mix_state_dir(home.path());
        tokio::fs::create_dir_all(&state_dir).await.unwrap();

        let committed = git()
            .sync(&user(home.path()), &state_dir, &CancellationToken::new())
            .await
            .unwrap();

        assert!(!committed);
    }

    #[tokio::test]
    async fn init_bounds_the_repository_with_a_gitignore() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = repository(home.path()).await;

        let contents = std::fs::read_to_string(state_dir.join(GITIGNORE)).unwrap();
        assert!(contents.contains("/*"));
        for file in [FLAKE_NIX, HOME_NIX, FLAKE_LOCK, GITIGNORE] {
            assert!(
                contents.contains(&format!("!/{file}")),
                "{file} should stay tracked"
            );
        }
    }

    #[tokio::test]
    async fn sync_commits_the_generated_config() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = repository(home.path()).await;
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();
        std::fs::write(state_dir.join(HOME_NIX), "home-content").unwrap();

        let committed = git()
            .sync(&user(home.path()), &state_dir, &CancellationToken::new())
            .await
            .unwrap();

        assert!(committed);
        assert_eq!(
            committed_files(&state_dir).await,
            vec![
                GITIGNORE.to_string(),
                FLAKE_NIX.to_string(),
                HOME_NIX.to_string()
            ]
        );
    }

    #[tokio::test]
    async fn sync_leaves_anything_that_is_not_generated_config_uncommitted() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = repository(home.path()).await;
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();
        std::fs::write(state_dir.join("id_rsa"), "a stray secret").unwrap();
        std::fs::write(state_dir.join("flake.nix~"), "an editor backup").unwrap();
        std::fs::create_dir_all(state_dir.join("result")).unwrap();
        std::fs::write(state_dir.join("result/out"), "build output").unwrap();

        git()
            .sync(&user(home.path()), &state_dir, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            committed_files(&state_dir).await,
            vec![GITIGNORE.to_string(), FLAKE_NIX.to_string()]
        );
    }

    #[tokio::test]
    async fn sync_commits_nothing_twice() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = repository(home.path()).await;
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();

        let git = git();
        assert!(
            git.sync(&user(home.path()), &state_dir, &CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(
            !git.sync(&user(home.path()), &state_dir, &CancellationToken::new())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn sync_commits_drift_in_the_generated_config() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = repository(home.path()).await;
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();
        let git = git();
        git.sync(&user(home.path()), &state_dir, &CancellationToken::new())
            .await
            .unwrap();

        std::fs::write(state_dir.join(FLAKE_NIX), "repaired-content").unwrap();

        assert!(
            git.sync(&user(home.path()), &state_dir, &CancellationToken::new())
                .await
                .unwrap()
        );
    }
}
