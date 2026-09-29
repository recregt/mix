//! The git repository the generated configuration is tracked in.
//!
//! Every write mix makes to a user's state directory is committed, so a reader can see what
//! changed and when. Which `git` that is depends on what the machine has: the one in the
//! profile mix installed, or the one that was already on the system.

use std::path::{Path, PathBuf};

use mix_core::Result;
use mix_core::paths::{FLAKE_LOCK, FLAKE_NIX, HOME_NIX, STATE_FILE};
use mix_core::privilege::InvokingUser;
use mix_exec::Scope;

use crate::effect::exec::{run_as, status_as};
use crate::effect::fs::exists;
use crate::effect::home;

const AUTHOR_NAME: &str = "mix";
const AUTHOR_EMAIL: &str = "mix@localhost";
const COMMIT_MESSAGE: &str = "mix: sync generated home-manager config";

const PROFILE_GIT: &str = ".nix-profile/bin/git";
const GITIGNORE: &str = ".gitignore";

const MANAGED_FILES: &[&str] = &[GITIGNORE, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, STATE_FILE];

const GITIGNORE_CONTENTS: &str = "# Managed by mix -- do not edit, changes are overwritten.\n/*\n!/.gitignore\n!/flake.lock\n!/flake.nix\n!/home.nix\n!/state\n";

pub struct Git {
    binary: String,
}

impl Git {
    pub async fn resolve(user: &InvokingUser, configured: Option<&Path>) -> Self {
        Self {
            binary: resolve_binary(
                user,
                configured.map(|path| path.to_string_lossy().into_owned()),
            )
            .await,
        }
    }

    pub async fn init(&self, user: &InvokingUser, state_dir: &Path, scope: &Scope) -> Result<()> {
        let state_dir_str = state_dir.to_string_lossy().into_owned();
        self.run_as(user, &["-C", &state_dir_str, "init", "-q"], scope)
            .await?;
        write_gitignore(state_dir, scope).await
    }

    pub async fn sync(&self, user: &InvokingUser, state_dir: &Path, scope: &Scope) -> Result<bool> {
        let git_dir = state_dir.join(".git");
        if !exists(&git_dir).await {
            return Ok(false);
        }

        let state_dir_str = state_dir.to_string_lossy().into_owned();
        let staged = self.stage(user, &state_dir_str, state_dir, scope).await?;
        if !staged {
            return Ok(false);
        }

        let clean = self
            .status_as(
                user,
                &["-C", &state_dir_str, "diff", "--cached", "--quiet"],
                scope,
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
            scope,
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
        scope: &Scope,
    ) -> Result<bool> {
        let mut args = vec!["-C", state_dir_str, "ls-files", "--"];
        args.extend(MANAGED_FILES);
        let tracked = self.run_as(user, &args, scope).await?;

        let mut paths: Vec<&str> = Vec::new();
        for file in MANAGED_FILES {
            if tracked.lines().any(|line| line == *file) || exists(state_dir.join(file)).await {
                paths.push(file);
            }
        }
        if paths.is_empty() {
            return Ok(false);
        }

        let mut args = vec!["-C", state_dir_str, "add", "-A", "--"];
        args.extend(&paths);
        self.run_as(user, &args, scope).await?;
        Ok(true)
    }

    async fn run_as(&self, user: &InvokingUser, args: &[&str], scope: &Scope) -> Result<String> {
        run_as(user, &self.binary, args, scope).await
    }

    async fn status_as(&self, user: &InvokingUser, args: &[&str], scope: &Scope) -> Result<bool> {
        status_as(user, &self.binary, args, scope).await
    }
}

async fn resolve_binary(user: &InvokingUser, configured: Option<String>) -> String {
    if let Some(binary) = configured.map(|path| path.trim().to_string())
        && !binary.is_empty()
    {
        return binary;
    }
    let profile_git = profile_git(user);
    if exists(&profile_git).await {
        return profile_git.to_string_lossy().into_owned();
    }
    "git".to_string()
}

fn profile_git(user: &InvokingUser) -> PathBuf {
    user.home.join(PROFILE_GIT)
}

async fn write_gitignore(state_dir: &Path, scope: &Scope) -> Result<()> {
    let path = state_dir.join(GITIGNORE);
    home::write_file(&path, GITIGNORE_CONTENTS.as_bytes(), 0o644, scope)
        .await
        .map_err(|failure| home::core_error(failure, &path))
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
        let path = std::env::var_os("PATH").unwrap_or_default();
        let binary = std::env::split_paths(&path)
            .map(|directory| directory.join("git"))
            .find(|candidate| candidate.is_file())
            .expect("the tests need git on PATH");
        Git {
            binary: binary.to_string_lossy().into_owned(),
        }
    }

    async fn repository(home: &Path) -> PathBuf {
        let state_dir = mix_core::paths::mix_state_dir(home);
        tokio::fs::create_dir_all(&state_dir).await.unwrap();
        git()
            .init(&user(home), &state_dir, &mix_exec::Scope::root())
            .await
            .unwrap();
        state_dir
    }

    async fn committed_files(state_dir: &Path) -> Vec<String> {
        let output = mix_exec::Command::new("git")
            .args(["-C", &state_dir.to_string_lossy(), "ls-files"])
            .output(&mix_exec::Scope::root())
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
            .sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
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
        for file in [FLAKE_NIX, HOME_NIX, FLAKE_LOCK, STATE_FILE, GITIGNORE] {
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
            .sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
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
            .sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
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
            git.sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
                .await
                .unwrap()
        );
        assert!(
            !git.sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
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
        git.sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
            .await
            .unwrap();

        std::fs::write(state_dir.join(FLAKE_NIX), "repaired-content").unwrap();

        assert!(
            git.sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
                .await
                .unwrap()
        );
    }
}
