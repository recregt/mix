use std::path::Path;

use mix_core::privilege::InvokingUser;
use mix_core::{CancellationToken, Result};

use crate::shared::os::{path_exists, run, run_as, status_as};

const AUTHOR_NAME: &str = "mix";
const AUTHOR_EMAIL: &str = "mix@localhost";
const COMMIT_MESSAGE: &str = "mix: sync generated home-manager config";

fn git_binary(user: &InvokingUser) -> String {
    user.home
        .join(".nix-profile/bin/git")
        .to_string_lossy()
        .into_owned()
}

pub async fn sync(
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
    let git = git_binary(user);

    run_as(
        user,
        &git,
        &[
            "-C",
            &state_dir_str,
            "add",
            "flake.nix",
            "home.nix",
            "flake.lock",
        ],
        token,
    )
    .await?;

    let clean = status_as(
        user,
        &git,
        &["-C", &state_dir_str, "diff", "--cached", "--quiet"],
        token,
    )
    .await?;
    if clean {
        return Ok(false);
    }

    run_as(
        user,
        &git,
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

pub async fn init(user: &InvokingUser, state_dir: &Path, token: &CancellationToken) -> Result<()> {
    let state_dir_str = state_dir.to_string_lossy().into_owned();
    run_as(
        user,
        &git_binary(user),
        &["-C", &state_dir_str, "init", "-q"],
        token,
    )
    .await?;
    Ok(())
}
