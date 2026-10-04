//! The git repository the generated configuration is tracked in.
//!
//! Every write mix makes to a user's state directory is committed, so a reader can see what
//! changed and when. Which `git` that is depends on what the machine has: the one in the
//! profile mix installed, or the one that was already on the system.

use std::path::{Path, PathBuf};

use mix_core::Result;
use mix_core::identity::InvokingUser;
use mix_core::paths::{FLAKE_LOCK, FLAKE_NIX, GIT_DIR, HOME_NIX, STATE_FILE};
use mix_exec::Scope;

use mix_exec::Command;

use crate::effect::exec::{command_as, run, status};
use crate::effect::fs::exists;
use crate::effect::home;

const AUTHOR_NAME: &str = "mix";
const AUTHOR_EMAIL: &str = "mix@localhost";
const COMMIT_MESSAGE: &str = "mix: sync generated home-manager config";

const PROFILE_GIT: &str = ".nix-profile/bin/git";
const GITIGNORE: &str = ".gitignore";

/// Settings given to every `git` call: no hook runs, no file-system monitor is started and no
/// commit is signed.
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

/// Checks a repository passes: `HEAD` resolves to a commit, every object of its tree reads, and
/// the index reads.
const VERIFY: &[&[&str]] = &[
    &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    &["archive", "--format=tar", "HEAD"],
    &["ls-files", "--stage"],
];

const MANAGED_FILES: &[&str] = &[GITIGNORE, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, STATE_FILE];

const GITIGNORE_CONTENTS: &str = "# Managed by mix -- do not edit, changes are overwritten.\n/*\n!/.gitignore\n!/flake.lock\n!/flake.nix\n!/home.nix\n!/state\n";

pub struct Git {
    binary: String,
}

impl Git {
    pub async fn resolve(user: &InvokingUser) -> Self {
        Self {
            binary: resolve_binary(user).await,
        }
    }

    pub async fn init(&self, user: &InvokingUser, state_dir: &Path, scope: &Scope) -> Result<()> {
        let state_dir_str = state_dir.to_string_lossy().into_owned();
        self.run_as(user, &["-C", &state_dir_str, "init", "-q"], scope)
            .await?;
        write_gitignore(state_dir, scope).await
    }

    /// Creates the repository and commits the generated config into it.
    pub async fn create(&self, user: &InvokingUser, state_dir: &Path, scope: &Scope) -> Result<()> {
        self.init(user, state_dir, scope).await?;
        self.sync(user, state_dir, scope).await.map(|_| ())
    }

    /// Whether `HEAD` resolves to a commit whose tree reads in full, and the index reads.
    ///
    /// Note: Only what a build reads is checked, so the cost follows the current tree, not the
    /// history. The repository is named with `--git-dir`, so a missing one is never answered by
    /// a repository further up.
    pub async fn verify(
        &self,
        user: &InvokingUser,
        repository: &Path,
        scope: &Scope,
    ) -> Result<bool> {
        let repository = repository.to_string_lossy();
        for check in VERIFY {
            let mut args = vec!["--git-dir", &repository];
            args.extend(check.iter());
            if !self.status_as(user, &args, scope).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn sync(&self, user: &InvokingUser, state_dir: &Path, scope: &Scope) -> Result<bool> {
        let git_dir = state_dir.join(GIT_DIR);
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

async fn write_gitignore(state_dir: &Path, scope: &Scope) -> Result<()> {
    let path = state_dir.join(GITIGNORE);
    home::write_file(&path, GITIGNORE_CONTENTS.as_bytes(), 0o644, scope)
        .await
        .map_err(|failure| home::core_error(failure, &path))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
pub(crate) mod testing {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use mix_core::identity::InvokingUser;

    use super::PROFILE_GIT;

    pub fn on_path() -> PathBuf {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path)
            .map(|directory| directory.join("git"))
            .find(|candidate| candidate.is_file())
            .expect("the tests need git on PATH")
    }

    /// The running account with `home` as its home, whose profile's `git` is the one on `PATH`,
    /// failing every `refused` subcommand.
    pub fn user(home: &Path, refused: Option<&str>) -> InvokingUser {
        let profile_git = home.join(PROFILE_GIT);
        std::fs::create_dir_all(profile_git.parent().unwrap()).unwrap();
        match refused {
            None => std::os::unix::fs::symlink(on_path(), &profile_git).unwrap(),
            Some(refused) => {
                std::fs::write(
                    &profile_git,
                    format!(
                        "#!/bin/sh\nfor arg; do [ \"$arg\" = {refused} ] && exit 1; done\nexec '{}' \"$@\"\n",
                        on_path().display()
                    ),
                )
                .unwrap();
                std::fs::set_permissions(&profile_git, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
        }
        InvokingUser {
            uid: nix::unistd::Uid::current().as_raw(),
            gid: nix::unistd::Gid::current().as_raw(),
            name: "mix-user".to_string(),
            home: home.to_path_buf(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::paths::INDEX_LOCK;

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
            binary: testing::on_path().to_string_lossy().into_owned(),
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

    fn hook(path: &Path, marker: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn sync_runs_no_hook_of_the_repository_or_the_user() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = repository(home.path()).await;
        let marker = home.path().join("hook-ran");
        hook(&state_dir.join(".git/hooks/pre-commit"), &marker);
        hook(&home.path().join("hooks/pre-commit"), &marker);
        std::fs::write(
            home.path().join(".gitconfig"),
            format!("[core]\n\thooksPath = {}/hooks\n", home.path().display()),
        )
        .unwrap();
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();

        let committed = git()
            .sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
            .await
            .unwrap();

        assert!(committed);
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn sync_commits_unsigned_whatever_the_user_configured() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = repository(home.path()).await;
        let settings = "[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = /bin/false\n";
        std::fs::write(home.path().join(".gitconfig"), settings).unwrap();
        std::fs::create_dir_all(home.path().join(".config/git")).unwrap();
        std::fs::write(home.path().join(".config/git/config"), settings).unwrap();
        let local = state_dir.join(".git/config");
        let mut config = std::fs::read_to_string(&local).unwrap();
        config.push_str(settings);
        std::fs::write(&local, config).unwrap();
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();

        let committed = git()
            .sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
            .await
            .unwrap();

        assert!(committed);
        assert_eq!(
            committed_files(&state_dir).await,
            vec![GITIGNORE.to_string(), FLAKE_NIX.to_string()]
        );
    }

    async fn committed(home: &Path) -> PathBuf {
        let state_dir = repository(home).await;
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();
        git()
            .sync(&user(home), &state_dir, &mix_exec::Scope::root())
            .await
            .unwrap();
        state_dir
    }

    async fn verified(home: &Path, state_dir: &Path) -> bool {
        git()
            .verify(
                &user(home),
                &state_dir.join(GIT_DIR),
                &mix_exec::Scope::root(),
            )
            .await
            .unwrap()
    }

    fn objects(repository: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(repository.join("objects"))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.file_name().is_some_and(|name| name.len() == 2))
            .flat_map(|fanout| {
                std::fs::read_dir(fanout)
                    .unwrap()
                    .filter_map(std::result::Result::ok)
            })
            .map(|entry| entry.path())
            .collect()
    }

    #[tokio::test]
    async fn a_committed_repository_verifies() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = committed(home.path()).await;

        assert!(verified(home.path(), &state_dir).await);
    }

    #[tokio::test]
    async fn a_repository_whose_commit_lost_its_tree_does_not_verify() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = committed(home.path()).await;
        let head = mix_exec::Command::new("git")
            .args(["-C", &state_dir.to_string_lossy(), "rev-parse", "HEAD"])
            .output(&mix_exec::Scope::root())
            .await
            .unwrap();
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        for object in objects(&state_dir.join(GIT_DIR)) {
            let name = format!(
                "{}{}",
                object
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy(),
                object.file_name().unwrap().to_string_lossy()
            );
            if name != head {
                std::fs::remove_file(object).unwrap();
            }
        }

        assert!(!verified(home.path(), &state_dir).await);
    }

    #[tokio::test]
    async fn a_repository_whose_head_does_not_resolve_does_not_verify() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = committed(home.path()).await;
        std::fs::write(state_dir.join(GIT_DIR).join("HEAD"), "garbage\n").unwrap();

        assert!(!verified(home.path(), &state_dir).await);
    }

    #[tokio::test]
    async fn a_repository_with_an_unreadable_config_does_not_verify() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = committed(home.path()).await;
        std::fs::write(state_dir.join(GIT_DIR).join("config"), "[[[ not a config\n").unwrap();

        assert!(!verified(home.path(), &state_dir).await);
    }

    #[tokio::test]
    async fn a_repository_with_an_unreadable_index_does_not_verify() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = committed(home.path()).await;
        std::fs::write(state_dir.join(GIT_DIR).join("index"), "garbage").unwrap();

        assert!(!verified(home.path(), &state_dir).await);
    }

    #[tokio::test]
    async fn a_repository_with_nothing_committed_does_not_verify() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = repository(home.path()).await;

        assert!(!verified(home.path(), &state_dir).await);
    }

    #[tokio::test]
    async fn a_missing_repository_is_not_answered_by_one_further_up() {
        let home = tempfile::tempdir().unwrap();
        let outer = committed(home.path()).await;
        let nested_home = outer.join("nested");
        let state_dir = mix_core::paths::mix_state_dir(&nested_home);
        std::fs::create_dir_all(&state_dir).unwrap();

        assert!(!verified(home.path(), &state_dir).await);
    }

    #[tokio::test]
    async fn create_commits_the_files_already_there() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = mix_core::paths::mix_state_dir(home.path());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join(FLAKE_NIX), "flake-content").unwrap();
        std::fs::write(state_dir.join(HOME_NIX), "home-content").unwrap();

        git()
            .create(&user(home.path()), &state_dir, &mix_exec::Scope::root())
            .await
            .unwrap();

        assert!(verified(home.path(), &state_dir).await);
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
    async fn sync_fails_while_a_stale_index_lock_is_left_behind() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = committed(home.path()).await;
        std::fs::write(state_dir.join(GIT_DIR).join(INDEX_LOCK), "").unwrap();
        std::fs::write(state_dir.join(FLAKE_NIX), "changed").unwrap();

        let synced = git()
            .sync(&user(home.path()), &state_dir, &mix_exec::Scope::root())
            .await;

        assert!(synced.is_err());
        assert!(verified(home.path(), &state_dir).await);
    }
}
