//! The git repository the generated configuration is recorded in.
//!
//! Note: mix drives `git` through plumbing commands only, as the user, with the `git` of the
//! user's Nix profile when it has one and the one on `PATH` otherwise.

use std::path::{Path, PathBuf};

use mix_core::Result;
use mix_core::identity::InvokingUser;
use mix_core::paths::{GIT_DIR, MANAGED_FILES};
use mix_exec::Scope;

use mix_exec::Command;

use crate::effect::exec::{command_as, exec_error, run, status};
use crate::effect::fs::exists;

const AUTHOR: &[(&str, &str)] = &[
    ("GIT_AUTHOR_NAME", "mix"),
    ("GIT_AUTHOR_EMAIL", "mix@localhost"),
    ("GIT_COMMITTER_NAME", "mix"),
    ("GIT_COMMITTER_EMAIL", "mix@localhost"),
];
const COMMIT_MESSAGE: &str = "mix: sync generated home-manager config";
const BRANCH: &str = "refs/heads/main";

const PROFILE_GIT: &str = ".nix-profile/bin/git";

/// Oldest `git` release mix runs, as major and minor version.
const OLDEST: (u32, u32) = (2, 34);

/// Settings given to every `git` call: no hook runs, no file-system monitor starts and no commit
/// is signed.
const ISOLATION: &[&str] = &[
    "core.hooksPath=/dev/null",
    "core.fsmonitor=false",
    "commit.gpgsign=false",
];

/// Environment of every `git` call besides the user's `HOME`, `USER` and `PATH`: no global or
/// system configuration is read and no credential is asked for.
const ISOLATED_ENV: &[(&str, &str)] = &[
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

pub struct Git {
    binary: String,
}

impl Git {
    pub async fn resolve(user: &InvokingUser, scope: &Scope) -> Result<Self> {
        let git = Self {
            binary: resolve_binary(user).await,
        };
        let reported = git.run_as(user, &["--version"], scope).await?;
        let found = reported.split_whitespace().nth(2).unwrap_or_default();
        let mut parts = found.split('.').map(str::parse::<u32>);
        match (parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor))) if (major, minor) >= OLDEST => Ok(git),
            _ => Err(mix_core::Error::Unsupported {
                program: git.binary,
                found: found.to_string(),
                oldest: format!("{}.{}", OLDEST.0, OLDEST.1),
            }),
        }
    }

    pub async fn init(&self, user: &InvokingUser, state_dir: &Path, scope: &Scope) -> Result<()> {
        let state_dir = state_dir.to_string_lossy();
        self.run_as(
            user,
            &[
                "init",
                "-q",
                "--initial-branch=main",
                "--template=",
                &state_dir,
            ],
            scope,
        )
        .await
        .map(|_| ())
    }

    /// Creates the repository and commits the generated config into it.
    pub async fn create(&self, user: &InvokingUser, state_dir: &Path, scope: &Scope) -> Result<()> {
        self.init(user, state_dir, scope).await?;
        self.sync(user, state_dir, scope).await.map(|_| ())
    }

    /// Whether the commit and tree of `HEAD` and every file in that tree match their object ids.
    ///
    /// Note: Only `HEAD`'s own objects are read, so the cost follows the current tree, not the
    /// history. The index is not checked, because every record rebuilds it. The repository is
    /// named with `--git-dir`, so a missing one is never answered by a repository further up.
    pub async fn verify(
        &self,
        user: &InvokingUser,
        repository: &Path,
        scope: &Scope,
    ) -> Result<bool> {
        let repository = repository.to_string_lossy();
        let tree = "HEAD^{tree}";
        let checks: [&[&str]; 2] = [
            &["rev-parse", "--verify", "--quiet", tree],
            &export("HEAD^!"),
        ];
        for check in checks {
            let mut args = vec!["--git-dir", &repository];
            args.extend(check);
            if !self.status_as(user, &args, scope).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn unrecorded(
        &self,
        user: &InvokingUser,
        state_dir: &Path,
        scope: &Scope,
    ) -> Result<Vec<String>> {
        let state_dir = state_dir.to_string_lossy();
        let mut args = vec![
            "--no-optional-locks",
            "-C",
            &state_dir,
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
        ];
        args.extend(MANAGED_FILES);
        Ok(self
            .run_as(user, &args, scope)
            .await?
            .lines()
            .filter_map(|line| line.trim_start().split_once(' '))
            .map(|(_, path)| path.trim_start().to_string())
            .collect())
    }

    pub async fn reusable(
        &self,
        user: &InvokingUser,
        state_dir: &Path,
        files: &[String],
        scope: &Scope,
    ) -> Result<bool> {
        let mut present = Vec::with_capacity(files.len());
        for file in files {
            if exists(state_dir.join(file)).await {
                present.push(file.as_str());
            }
        }
        if present.is_empty() {
            return Ok(true);
        }
        let state_dir = state_dir.to_string_lossy();
        let mut args = vec!["-C", &state_dir, "hash-object", "--"];
        args.extend(&present);
        for object in self.run_as(user, &args, scope).await?.lines() {
            let blob = format!("{object}^{{blob}}");
            let reads = ["-C", &state_dir, "rev-parse", "--verify", "--quiet", &blob];
            if !self.status_as(user, &reads, scope).await?
                && self
                    .status_as(user, &["-C", &state_dir, "cat-file", "-e", object], scope)
                    .await?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn sync(&self, user: &InvokingUser, state_dir: &Path, scope: &Scope) -> Result<bool> {
        if !exists(state_dir.join(GIT_DIR)).await {
            return Ok(false);
        }
        let state_dir = state_dir.to_string_lossy();
        let dir = ["-C", &*state_dir];

        let rebuild = [&dir[..], &["read-tree", "--empty"]].concat();
        self.run_as(user, &rebuild, scope).await?;
        let mut stage = dir.to_vec();
        stage.extend(["update-index", "--add", "--remove", "--"]);
        stage.extend(MANAGED_FILES);
        self.run_as(user, &stage, scope).await?;

        let tree = self
            .run_as(user, &[&dir[..], &["write-tree"]].concat(), scope)
            .await?;
        let parent = self
            .found(user, &dir, &format!("{BRANCH}^{{commit}}"), scope)
            .await?;
        match &parent {
            Some(parent) => {
                let recorded = self
                    .found(user, &dir, &format!("{parent}^{{tree}}"), scope)
                    .await?;
                if recorded.as_deref() == Some(tree.as_str()) {
                    return Ok(false);
                }
            }
            None => {
                let listed = [&dir[..], &["ls-files"]].concat();
                if self.run_as(user, &listed, scope).await?.is_empty() {
                    return Ok(false);
                }
            }
        }

        let mut commit = dir.to_vec();
        commit.extend(["commit-tree", &tree, "-m", COMMIT_MESSAGE]);
        if let Some(parent) = &parent {
            commit.extend(["-p", parent]);
        }
        let command = AUTHOR
            .iter()
            .fold(self.command(user, &commit), |command, (key, value)| {
                command.env(key, value)
            });
        let commit = run(command, scope).await?;

        let tree = format!("{commit}^{{tree}}");
        let only = format!("{commit}^!");
        self.run_as(
            user,
            &[&dir[..], &["rev-parse", "--verify", "--quiet", &tree]].concat(),
            scope,
        )
        .await?;
        self.run_as(user, &[&dir[..], &export(&only)].concat(), scope)
            .await?;

        let old = parent.as_deref().unwrap_or("");
        let publish = [
            &dir[..],
            &["update-ref", "-m", COMMIT_MESSAGE, BRANCH, &commit, old],
        ]
        .concat();
        self.run_as(user, &publish, scope).await?;
        Ok(true)
    }

    async fn found(
        &self,
        user: &InvokingUser,
        dir: &[&str],
        rev: &str,
        scope: &Scope,
    ) -> Result<Option<String>> {
        let args = [dir, &["rev-parse", "--verify", "--quiet", rev]].concat();
        let output = self
            .command(user, &args)
            .output(scope)
            .await
            .map_err(exec_error)?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
    }

    async fn run_as(&self, user: &InvokingUser, args: &[&str], scope: &Scope) -> Result<String> {
        run(self.command(user, args), scope).await
    }

    async fn status_as(&self, user: &InvokingUser, args: &[&str], scope: &Scope) -> Result<bool> {
        status(self.command(user, args), scope).await
    }

    /// A `git` run as `user` that reads no configuration, hook or credential of the user's.
    ///
    /// Note: The `-c` overrides also win over the repository's own `.git/config`.
    fn command(&self, user: &InvokingUser, args: &[&str]) -> Command {
        let mut isolated: Vec<&str> = ISOLATION
            .iter()
            .flat_map(|setting| ["-c", setting])
            .collect();
        isolated.extend(args);
        let command = command_as(user, &self.binary, &isolated);
        ISOLATED_ENV
            .iter()
            .fold(command, |command, (key, value)| command.env(key, value))
    }
}

fn export(only: &str) -> [&str; 4] {
    [
        "fast-export",
        "--full-tree",
        "--reference-excluded-parents",
        only,
    ]
}

async fn resolve_binary(user: &InvokingUser) -> String {
    let profile_git = profile_git(user);
    if exists(&profile_git).await {
        return profile_git.to_string_lossy().into_owned();
    }
    "git".to_string()
}

fn profile_git(user: &InvokingUser) -> PathBuf {
    user.home.join(PROFILE_GIT)
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

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn resolve_binary_uses_the_git_in_the_users_nix_profile() {
        let home = tempfile::tempdir().unwrap();
        let profile_git = profile_git(&user(home.path()));
        std::fs::create_dir_all(profile_git.parent().unwrap()).unwrap();
        std::fs::write(&profile_git, "").unwrap();

        let binary = resolve_binary(&user(home.path())).await;

        assert_eq!(binary, profile_git.to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_binary_falls_back_to_the_path() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(resolve_binary(&user(home.path())).await, "git");
    }
}
